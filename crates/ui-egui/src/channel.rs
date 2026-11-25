//! The app id every LightCraft OS window carries and the channel name it shows.
//!
//! The identity is selected once, before any UI service is created. Stable keeps the original id,
//! name and version line; Nightly uses its own window/Wayland id and its own name, so a dock, an
//! Alt-Tab list or a window rule can tell the two apart, and About never claims to be Stable.
//! The package/bundle identity (`ai.storyteller.lightcraft.nightly`) is a different string: it names
//! the installed artifact, not a window.

/// Matches the `.desktop` file and hicolor icon name, so Wayland docks pick up the icon.
pub const APP_ID: &str = "ai.storyteller.lightcraft";
/// The window/Wayland id and (on Windows) the AppUserModelID of the Nightly channel.
pub const NIGHTLY_APP_ID: &str = "ai.storyteller.lightcraft.nightly-v1";
pub const APP_NAME: &str = "LightCraft";
pub const NIGHTLY_APP_NAME: &str = "LightCraft Nightly";

/// Immutable startup identity. The desktop selects it before any UI/runtime service is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchIdentity {
    Stable,
    NightlyV1,
}

static IDENTITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
static COMMIT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn select_identity(identity: LaunchIdentity) -> Result<(), String> {
    let value = match identity {
        LaunchIdentity::Stable => 1,
        LaunchIdentity::NightlyV1 => 2,
    };
    IDENTITY
        .compare_exchange(0, value, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire)
        .map(|_| ())
        .map_err(|_| "the window launch identity was already selected".into())
}

pub fn identity() -> LaunchIdentity {
    if IDENTITY.load(std::sync::atomic::Ordering::Acquire) == 2 { LaunchIdentity::NightlyV1 } else { LaunchIdentity::Stable }
}

pub fn is_nightly() -> bool {
    identity() == LaunchIdentity::NightlyV1
}

pub const fn id_for(identity: LaunchIdentity) -> &'static str {
    match identity {
        LaunchIdentity::Stable => APP_ID,
        LaunchIdentity::NightlyV1 => NIGHTLY_APP_ID,
    }
}

pub fn app_id() -> &'static str {
    id_for(identity())
}

pub const fn name_for(identity: LaunchIdentity) -> &'static str {
    match identity {
        LaunchIdentity::Stable => APP_NAME,
        LaunchIdentity::NightlyV1 => NIGHTLY_APP_NAME,
    }
}

/// The channel's name for OS surfaces (title bar, task switcher, menus).
pub fn name() -> &'static str {
    name_for(identity())
}

/// The version the app was built as: the Nightly feed version (set by the desktop host) or the
/// Cargo package version of a stable build.
pub fn version() -> &'static str {
    VERSION.get().map_or(env!("CARGO_PKG_VERSION"), String::as_str)
}

/// The source commit, when the host knows it (Nightly builds only).
pub fn commit() -> Option<&'static str> {
    COMMIT.get().map(String::as_str)
}

pub fn select_version(version: String, commit: Option<String>) -> Result<(), String> {
    VERSION.set(version).map_err(|_| "the channel version was already selected".to_string())?;
    if let Some(commit) = commit.filter(|c| !c.is_empty()) {
        let _ = COMMIT.set(commit);
    }
    Ok(())
}

/// The version line About and the window show: `0.21.0` or `0.21.0 (nightly-v1)`.
pub fn display_version() -> String {
    match identity() {
        LaunchIdentity::Stable => version().to_owned(),
        LaunchIdentity::NightlyV1 => format!("{} (nightly-v1)", version()),
    }
}

/// The title-bar caption: the channel name, then the open collection when one is named.
pub fn window_title(collection: Option<&str>) -> String {
    match collection.filter(|name| !name.is_empty()) {
        Some(name) => format!("{} \u{2014} {}", name_owned(), name),
        None => name_owned(),
    }
}

fn name_owned() -> String {
    name().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_channels_never_share_a_window_identity_or_a_name() {
        assert_eq!(id_for(LaunchIdentity::Stable), APP_ID);
        assert_eq!(id_for(LaunchIdentity::NightlyV1), NIGHTLY_APP_ID);
        assert_ne!(id_for(LaunchIdentity::Stable), id_for(LaunchIdentity::NightlyV1));
        assert_ne!(name_for(LaunchIdentity::Stable), name_for(LaunchIdentity::NightlyV1));
        // The window id is not the artifact id: the package/bundle one has no -v1.
        assert!(NIGHTLY_APP_ID.ends_with(".nightly-v1"));
        assert!(NIGHTLY_APP_ID.trim_end_matches("-v1").ends_with(".nightly"));
    }

    #[test]
    fn an_unselected_channel_is_stable_and_a_caption_carries_the_channel_name() {
        assert_eq!(identity(), LaunchIdentity::Stable);
        assert!(!is_nightly());
        assert_eq!(name(), APP_NAME);
        assert_eq!(window_title(None), "LightCraft");
        assert_eq!(window_title(Some("")), "LightCraft");
        assert_eq!(window_title(Some("Wedding")), "LightCraft \u{2014} Wedding");
        assert_eq!(display_version(), env!("CARGO_PKG_VERSION"));
    }
}
