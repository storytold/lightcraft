//! The product's name and identity, generated at build time from `brand.toml` at the workspace root
//! (or `$BRAND_FILE`). No source file names the product: they all read it from here.
//!
//! - `[product]` keys (`DISPLAY_NAME`, `BINARY`, `ENV_PREFIX`, …) are the user-visible brand.
//! - `[identity]` keys (`APP_ID`, `SETTINGS_DIR`, …) are OS and file identity; the previous values live in
//!   `[legacy]` (`LEGACY_SETTINGS_DIRS`, …) and are migrated from on first start.
//! - `[stable]` keys (`XMP_NAMESPACE_URI`, …) never follow the name.
//! - `[updates]` `public_keys` is [`UPDATES_PUBLIC_KEYS`]: the key a release's checksum list is signed with (two
//!   while it is rotated), as text (empty when the brand file has none). They are parsed where they are used
//!   (`lcc-updater`), and a key that does not parse is treated like none: nothing is installed.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::PathBuf;
use std::sync::Mutex;

include!(concat!(env!("OUT_DIR"), "/brand.rs"));

/// `{prefix}_{name}`, e.g. `env_var("LOG")` is `<ENV_PREFIX>_LOG`.
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

/// Where the app keeps its settings and library on a platform with no home folder to derive them
/// from (Android, whose app-private folders only the activity knows). Set once by the host at start-up,
/// before anything asks for a folder; it then wins over the environment-based defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformDirs {
    /// Replaces the platform config root: the settings folder is `<config_root>/<settings_dir>`.
    pub config_root: PathBuf,
    /// Replaces `~/Pictures`: the default library is `<pictures>/<library_default>`.
    pub pictures: PathBuf,
}

static PLATFORM_DIRS: std::sync::OnceLock<PlatformDirs> = std::sync::OnceLock::new();

/// Sets the [`PlatformDirs`] for this process. Only the first call counts; returns whether this one did.
pub fn set_platform_dirs(dirs: PlatformDirs) -> bool {
    PLATFORM_DIRS.set(dirs).is_ok()
}

/// The folders the host set with [`set_platform_dirs`], if any.
pub fn platform_dirs() -> Option<&'static PlatformDirs> {
    PLATFORM_DIRS.get()
}

/// Whether this process is a test binary: Cargo names every crate's unit and integration test binaries
/// `<crate>-<hash>`, a name no app or tool it builds for running has.
pub fn is_test_binary() -> bool {
    static TEST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TEST.get_or_init(|| {
        cfg!(test)
            || std::env::current_exe().ok().is_some_and(|exe| {
                // cargo names test binaries `<crate>-<16 hex digits>`, wherever its layout puts them
                // (`target/<profile>/deps/` or `<build-dir>/<profile>/build/<crate>/<hash>/out/`)
                exe.file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.rsplit_once('-'))
                    .is_some_and(|(_, hash)| hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            })
    })
}

/// The settings root a test binary uses instead of the user's: a folder of its own in the temp folder, so no test
/// reads or writes the real settings, whatever it forgets to override.
fn test_config_root() -> PathBuf {
    std::env::temp_dir().join("lcc-test-settings").join(std::process::id().to_string())
}

/// The platform config root: the host's [`PlatformDirs`], else `~/Library/Application Support`,
/// `%APPDATA%`, or `$XDG_CONFIG_HOME` / `~/.config`. In a test binary ([`is_test_binary`]), a temporary folder.
fn config_root() -> Option<PathBuf> {
    if is_test_binary() {
        return Some(test_config_root());
    }
    if let Some(d) = platform_dirs() {
        Some(d.config_root.clone())
    } else if cfg!(target_os = "macos") {
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
/// Unixes (`~/.config/<settings_dir>`).
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
    if !is_unmigrated(current) {
        return Ok(None);
    }
    let Some(old) = legacy.iter().find(|d| d.is_dir() && d.as_path() != current) else { return Ok(None) };
    let existed = current.exists();
    if let Err(e) = copy_dir(old, current) {
        // a folder that already held the log file stays; only a fresh one is removed
        if !existed {
            let _ = std::fs::remove_dir_all(current);
        }
        return Err(e);
    }
    std::fs::write(current.join(MIGRATED_MARKER), format!("{}\n", old.display()))?;
    Ok(Some(old.clone()))
}

/// True when `current` is absent, or holds nothing but the `logs` folder (a start that opened its log file
/// before the copy ran must not hide the legacy settings) and was not already migrated.
fn is_unmigrated(current: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(current) else { return !current.exists() };
    entries.flatten().all(|e| e.file_name() == "logs")
}

/// One settings file a previous name kept, for a settings folder that already exists (so
/// [`migrate_legacy_settings`] left it alone): when the settings folder has no `name`, **copy** it from the
/// first legacy folder that has it (never moved, never overwriting). Returns the legacy file that was copied.
pub fn migrate_legacy_settings_file(name: &str) -> std::io::Result<Option<PathBuf>> {
    let Some(current) = settings_dir() else { return Ok(None) };
    migrate_file_from(&current, &legacy_settings_dirs(), name)
}

/// [`migrate_legacy_settings_file`] with explicit folders (for tests and tools). `legacy` is searched in order;
/// a folder that is `current` itself is skipped. An existing `current/name` (file, folder or anything else) is
/// never touched. Errors are returned, never panicked on; a half-written copy is removed.
pub fn migrate_file_from(current: &std::path::Path, legacy: &[PathBuf], name: &str) -> std::io::Result<Option<PathBuf>> {
    let dest = current.join(name);
    if std::fs::symlink_metadata(&dest).is_ok() {
        return Ok(None);
    }
    let Some(from) = legacy.iter().filter(|d| d.as_path() != current).map(|d| d.join(name)).find(|f| f.is_file()) else {
        return Ok(None);
    };
    std::fs::create_dir_all(current)?;
    let mut src = std::fs::File::open(&from)?;
    // `create_new`: a file that appeared since the check above is still never overwritten
    let mut out = match std::fs::OpenOptions::new().write(true).create_new(true).open(&dest) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
        Err(e) => return Err(e),
    };
    if let Err(e) = std::io::copy(&mut src, &mut out).and_then(|_| out.sync_all()) {
        drop(out);
        let _ = std::fs::remove_file(&dest);
        return Err(e);
    }
    Ok(Some(from))
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

/// Replaces the `{app}` placeholder (localisation catalogs, templates) with [`DISPLAY_NAME`], and `{upstream}` (the
/// few texts that name the project this one is forked from, such as its library format) with [`UPSTREAM_NAME`]. A brand
/// with no `[upstream]` has an empty name, so those texts read without it.
pub fn fill(text: &str) -> String {
    text.replace("{app}", DISPLAY_NAME).replace("{upstream}", UPSTREAM_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No test reads or writes the user's settings: in a test binary the settings folder is a temporary one.
    #[test]
    fn tests_never_use_the_real_settings_folder() {
        assert!(is_test_binary());
        let dir = settings_dir().unwrap();
        assert!(dir.starts_with(std::env::temp_dir()), "{}", dir.display());
        assert!(legacy_settings_dirs().iter().all(|d| d.starts_with(std::env::temp_dir())));
    }

    #[test]
    fn settings_folder_migration() {
        // migration copies and marks
        {
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
        // migration runs when only the log folder exists
        {
            let base = std::env::temp_dir().join(format!("brand-mig-logs-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let old = base.join("old");
            std::fs::create_dir_all(&old).unwrap();
            std::fs::write(old.join("settings.json"), "{}").unwrap();
            let new = base.join("new");
            std::fs::create_dir_all(new.join("logs")).unwrap();
            std::fs::write(new.join("logs/app.log"), "x").unwrap();
            assert!(migrate_settings_from(&new, std::slice::from_ref(&old)).unwrap().is_some());
            assert!(new.join("settings.json").exists());
            assert!(new.join("logs/app.log").exists());
            // settings of its own: left alone
            std::fs::write(old.join("other.json"), "{}").unwrap();
            assert_eq!(migrate_settings_from(&new, &[old]).unwrap(), None);
            assert!(!new.join("other.json").exists());
            let _ = std::fs::remove_dir_all(&base);
        }
    }

    /// A fresh temporary folder for one test (never the real settings folders).
    fn file_mig_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("brand-file-mig-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn single_file_migration() {
        // file migration copies from the first legacy folder that has it
        {
            let base = file_mig_base("copy");
            let (first, second, third) = (base.join("first"), base.join("second"), base.join("third"));
            // the first legacy folder exists but lacks the file; the second and third have one each
            std::fs::create_dir_all(&first).unwrap();
            std::fs::create_dir_all(&second).unwrap();
            std::fs::create_dir_all(&third).unwrap();
            std::fs::write(second.join("surfaces.json"), "from second").unwrap();
            std::fs::write(third.join("surfaces.json"), "from third").unwrap();
            // the settings folder exists already (so the folder migration skipped it) with other settings
            let new = base.join("new");
            std::fs::create_dir_all(&new).unwrap();
            std::fs::write(new.join("ui.json"), "{}").unwrap();
            let legacy = [base.join("missing"), first, second.clone(), third];
            let got = migrate_file_from(&new, &legacy, "surfaces.json").unwrap();
            assert_eq!(got, Some(second.join("surfaces.json")));
            assert_eq!(std::fs::read_to_string(new.join("surfaces.json")).unwrap(), "from second");
            assert!(second.join("surfaces.json").exists(), "the legacy file is kept");
            assert_eq!(std::fs::read_to_string(new.join("ui.json")).unwrap(), "{}");
            // next start: the copy is the app's own now
            assert_eq!(migrate_file_from(&new, &legacy, "surfaces.json").unwrap(), None);
            // a settings folder that doesn't exist yet is created
            let fresh = base.join("fresh");
            assert!(migrate_file_from(&fresh, &legacy, "surfaces.json").unwrap().is_some());
            assert_eq!(std::fs::read_to_string(fresh.join("surfaces.json")).unwrap(), "from second");
            let _ = std::fs::remove_dir_all(&base);
        }
        // file migration never overwrites
        {
            let base = file_mig_base("keep");
            let old = base.join("old");
            std::fs::create_dir_all(&old).unwrap();
            std::fs::write(old.join("surfaces.json"), "legacy").unwrap();
            let new = base.join("new");
            std::fs::create_dir_all(&new).unwrap();
            std::fs::write(new.join("surfaces.json"), "mine").unwrap();
            assert_eq!(migrate_file_from(&new, std::slice::from_ref(&old), "surfaces.json").unwrap(), None);
            assert_eq!(std::fs::read_to_string(new.join("surfaces.json")).unwrap(), "mine");
            // an empty file is still the app's own
            std::fs::write(new.join("surfaces.json"), "").unwrap();
            assert_eq!(migrate_file_from(&new, std::slice::from_ref(&old), "surfaces.json").unwrap(), None);
            assert_eq!(std::fs::read_to_string(new.join("surfaces.json")).unwrap(), "");
            // so is a folder in its place
            let other = base.join("other");
            std::fs::create_dir_all(other.join("surfaces.json")).unwrap();
            assert_eq!(migrate_file_from(&other, std::slice::from_ref(&old), "surfaces.json").unwrap(), None);
            assert!(other.join("surfaces.json").is_dir());
            let _ = std::fs::remove_dir_all(&base);
        }
        // file migration does nothing without a legacy file
        {
            let base = file_mig_base("none");
            let old = base.join("old");
            // the legacy folder has other settings, and a folder (not a file) by the name
            std::fs::create_dir_all(old.join("surfaces.json")).unwrap();
            std::fs::write(old.join("ui.json"), "{}").unwrap();
            let new = base.join("new");
            let legacy = [base.join("missing"), old];
            assert_eq!(migrate_file_from(&new, &legacy, "surfaces.json").unwrap(), None);
            assert!(!new.exists(), "no settings folder is created for nothing");
            // no legacy folders at all
            assert_eq!(migrate_file_from(&new, &[], "surfaces.json").unwrap(), None);
            // the settings folder listed as a legacy one is not copied onto itself
            std::fs::create_dir_all(&new).unwrap();
            assert_eq!(migrate_file_from(&new, std::slice::from_ref(&new), "surfaces.json").unwrap(), None);
            assert!(!new.join("surfaces.json").exists());
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}
