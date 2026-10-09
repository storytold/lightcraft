//! The product's name and identity, generated at build time from `brand.toml` at the workspace root
//! (or `$BRAND_FILE`). No source file names the product: they all read it from here.
//!
//! - `[product]` keys (`DISPLAY_NAME`, `BINARY`, `ENV_PREFIX`, …) are the user-visible brand.
//! - `[identity]` keys (`APP_ID`, `SETTINGS_DIR`, …) are OS and file identity; the previous values live in
//!   `[legacy]` (`LEGACY_SETTINGS_DIRS`, …) and are migrated from on first start.
//! - `[stable]` keys (`XMP_NAMESPACE_URI`, …) never follow the name.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/brand.rs"));

/// `{prefix}_{name}`, e.g. `env_var("LOG")` is `NONAMEYET_LOG`.
pub fn env_var(name: &str) -> String {
    format!("{ENV_PREFIX}_{name}")
}

/// Reads `{ENV_PREFIX}_{name}`, falling back to each legacy prefix in order. A legacy hit logs a one-time
/// deprecation line (through `log`-free stderr, as this crate has no dependencies).
pub fn env(name: &str) -> Option<String> {
    env_os(name).and_then(|v| v.into_string().ok())
}

/// [`env`] for values that may not be UTF-8 (paths).
pub fn env_os(name: &str) -> Option<std::ffi::OsString> {
    if let Some(v) = std::env::var_os(env_var(name)) {
        return Some(v);
    }
    for prefix in LEGACY_ENV_PREFIXES {
        let legacy = format!("{prefix}_{name}");
        if let Some(v) = std::env::var_os(&legacy) {
            warn_legacy_env(&legacy, &env_var(name));
            return Some(v);
        }
    }
    None
}

/// Whether `{ENV_PREFIX}_{name}` (or a legacy spelling) is set at all.
pub fn env_is_set(name: &str) -> bool {
    env_os(name).is_some()
}

fn warn_legacy_env(legacy: &str, current: &str) {
    static WARNED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let Ok(mut warned) = WARNED.lock() else { return };
    if !warned.iter().any(|w| w == legacy) {
        warned.push(legacy.to_string());
        eprintln!("warning: environment variable {legacy} is deprecated; use {current}");
    }
}

/// The platform config root: `~/Library/Application Support`, `%APPDATA%`, or `$XDG_CONFIG_HOME` / `~/.config`.
fn config_root() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    }
}

/// The folder name for `name` on this platform: as written on macOS and Windows, lower-case on other
/// Unixes (`~/.config/nonameyet`).
pub fn settings_dir_name(name: &str) -> String {
    if cfg!(any(target_os = "macos", windows)) { name.to_string() } else { name.to_lowercase() }
}

/// The per-user settings folder (`…/<settings_dir>`), if the platform says where.
pub fn settings_dir() -> Option<PathBuf> {
    config_root().map(|r| r.join(settings_dir_name(SETTINGS_DIR)))
}

/// Settings folders of previous identities, in `[legacy]` order, for migration.
pub fn legacy_settings_dirs() -> Vec<PathBuf> {
    let Some(root) = config_root() else { return Vec::new() };
    LEGACY_SETTINGS_DIRS.iter().map(|d| root.join(settings_dir_name(d))).collect()
}

/// Name of the marker file written into a settings folder copied from a legacy one.
pub const MIGRATED_MARKER: &str = "migrated-from";

/// First-start migration: if the settings folder does not exist and a legacy one does, **copy** the legacy
/// folder (never move it) and write a [`MIGRATED_MARKER`] naming where it came from. Returns the legacy
/// folder that was copied. Errors are returned, never panicked on; a half-copied folder is removed.
pub fn migrate_legacy_settings() -> std::io::Result<Option<PathBuf>> {
    let Some(current) = settings_dir() else { return Ok(None) };
    migrate_settings_from(&current, &legacy_settings_dirs())
}

/// [`migrate_legacy_settings`] with explicit folders (for tests and tools).
pub fn migrate_settings_from(current: &std::path::Path, legacy: &[PathBuf]) -> std::io::Result<Option<PathBuf>> {
    if current.exists() {
        return Ok(None);
    }
    let Some(old) = legacy.iter().find(|d| d.is_dir() && d.as_path() != current) else { return Ok(None) };
    if let Err(e) = copy_dir(old, current) {
        let _ = std::fs::remove_dir_all(current);
        return Err(e);
    }
    std::fs::write(current.join(MIGRATED_MARKER), format!("{}\n", old.display()))?;
    Ok(Some(old.clone()))
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = to.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &dest)?;
        }
        // symlinks and special files are skipped: settings folders hold plain files
    }
    Ok(())
}

/// Every preset extension this build reads: the current one first, then legacy ones.
pub fn preset_exts() -> impl Iterator<Item = &'static str> {
    std::iter::once(PRESET_EXT).chain(LEGACY_PRESET_EXTS.iter().copied())
}

/// Every catalog extension this build reads: the current one first, then legacy ones.
pub fn catalog_exts() -> impl Iterator<Item = &'static str> {
    std::iter::once(CATALOG_EXT).chain(LEGACY_CATALOG_EXTS.iter().copied())
}

/// Default library folder names, current first, then one per legacy settings name (`<legacy> Library`).
pub fn library_default_names() -> Vec<String> {
    let mut v = vec![LIBRARY_DEFAULT.to_string()];
    v.extend(LEGACY_SETTINGS_DIRS.iter().map(|d| format!("{d} Library")));
    v
}

/// Replaces the `{app}` placeholder (localisation catalogs, templates) with [`DISPLAY_NAME`].
pub fn fill(text: &str) -> String {
    text.replace("{app}", DISPLAY_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_set() {
        assert!(!DISPLAY_NAME.is_empty());
        assert!(!BINARY.is_empty() && !BINARY.contains(' '));
        assert!(ENV_PREFIX.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'));
        assert!(APP_ID.contains('.'));
    }

    #[test]
    fn env_var_uses_prefix() {
        assert_eq!(env_var("LOG"), format!("{ENV_PREFIX}_LOG"));
    }

    #[test]
    fn fill_replaces_placeholder() {
        assert_eq!(fill("About {app}"), format!("About {DISPLAY_NAME}"));
    }

    #[test]
    fn migration_copies_and_marks() {
        let base = std::env::temp_dir().join(format!("brand-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let old = base.join("old");
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("settings.json"), "{}").unwrap();
        std::fs::write(old.join("sub/x"), "x").unwrap();
        let new = base.join("new");
        let got = migrate_settings_from(&new, &[base.join("missing"), old.clone()]).unwrap();
        assert_eq!(got.as_deref(), Some(old.as_path()));
        assert!(old.join("settings.json").exists(), "legacy folder is kept");
        assert_eq!(std::fs::read_to_string(new.join("sub/x")).unwrap(), "x");
        assert!(new.join(MIGRATED_MARKER).exists());
        // second start: nothing to do
        assert_eq!(migrate_settings_from(&new, &[old]).unwrap(), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
