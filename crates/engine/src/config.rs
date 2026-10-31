//! Where LightCraft keeps per-user data on this machine: the config folder (app settings, camera
//! profiles, face and denoise models).
//!
//! [`config_dir`] picks it, in order:
//! 1. `$LIGHTCRAFT_CONFIG_DIR`, when set (tests, agents, custom setups).
//! 2. **Portable mode** (issue #1200, as in PhotoCraft): a marker file ([`PORTABLE_MARKERS`]) beside the
//!    executable puts everything in `<exe dir>/LightCraftData`, so a copy on a USB stick leaves nothing in
//!    `%APPDATA%`. If that folder can't be written (read-only medium, Program Files), a warning is logged
//!    and the platform folder is used.
//! 3. The platform convention: macOS `~/Library/Application Support/LightCraft`, Windows
//!    `%APPDATA%\LightCraft`, Linux `$XDG_CONFIG_HOME/lightcraft` or `~/.config/lightcraft`. An empty or
//!    relative `HOME`, `APPDATA` or `XDG_CONFIG_HOME` is ignored, so the folder is never under the launch
//!    directory; with no usable one there is no settings folder.
//!
//! Hosts that have a file system (the desktop app, the CLI, the MCP server) share these defaults so a model
//! installed from one is there for the others. Nothing here is applied automatically: a [`Session`] has no
//! face-, denoise- or SAM 3 model folder until a host asks for one ([`Session::with_default_face_models`],
//! [`Session::with_default_denoise_models`], [`Session::with_default_sam3_model`]), so tests stay hermetic.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::Session;

/// Files beside the executable that switch on portable mode (either one; contents are ignored).
pub const PORTABLE_MARKERS: [&str; 2] = ["portable.txt", "LightCraft.portable"];
/// The data folder created beside the executable in portable mode.
pub const PORTABLE_DATA_DIR: &str = "LightCraftData";

#[derive(Clone, Copy)]
enum Platform {
    Mac,
    Windows,
    Other,
}

/// LightCraft's settings folder. A nonempty `$LIGHTCRAFT_CONFIG_DIR` overrides portable mode and the
/// platform default. The override is the complete directory; no application-name suffix is appended. An
/// explicitly empty value disables the settings directory instead of falling back to a personal profile.
pub fn config_dir() -> Option<PathBuf> {
    let platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Other
    };
    config_dir_with(platform, |name| std::env::var_os(name), portable_dir)
}

/// `portable` is asked only without an override, so an isolated run never probes beside the executable.
/// An empty or relative `HOME`, `APPDATA` or `XDG_CONFIG_HOME` is ignored (an unusable `XDG_CONFIG_HOME`
/// gives way to `HOME`): it would put the settings folder, and the logs and crash log in it, under
/// whatever directory LightCraft was started in (issue #2572).
fn config_dir_with(platform: Platform, env: impl Fn(&str) -> Option<OsString>, portable: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    if let Some(dir) = env("LIGHTCRAFT_CONFIG_DIR") {
        return (!dir.is_empty()).then(|| PathBuf::from(dir));
    }
    if let Some(dir) = portable() {
        return Some(dir);
    }
    // Only a fully qualified folder: an empty or relative one would be under the launch directory,
    // and on Windows `\foo` or `C:foo` would follow the current drive or its folder (#2572).
    let folder = |name: &str| env(name).filter(|dir| fully_qualified(platform, dir)).map(PathBuf::from);
    match platform {
        Platform::Mac => folder("HOME").map(|h| h.join("Library/Application Support/LightCraft")),
        Platform::Windows => folder("APPDATA").map(|a| a.join("LightCraft")),
        Platform::Other => folder("XDG_CONFIG_HOME").or_else(|| folder("HOME").map(|h| h.join(".config"))).map(|c| c.join("lightcraft")),
    }
}

/// Whether `dir` names one folder whatever the current drive and directory, by `platform`'s rules
/// rather than the host's, so each arm is tested the same on every host: on Windows a drive path
/// (`C:\…`) or a UNC or device path (`\\server\share`, `\\?\…`), elsewhere a path from `/`.
fn fully_qualified(platform: Platform, dir: &OsStr) -> bool {
    let text = dir.to_string_lossy();
    let b = text.as_bytes();
    let sep = |c: &u8| *c == b'\\' || *c == b'/';
    match platform {
        Platform::Windows => match (b.first(), b.get(1), b.get(2)) {
            (Some(drive), Some(b':'), Some(c)) => drive.is_ascii_alphabetic() && sep(c),
            (Some(a), Some(c), Some(d)) => sep(a) && sep(c) && !sep(d),
            _ => false,
        },
        Platform::Mac | Platform::Other => b.first() == Some(&b'/'),
    }
}

/// This process's portable data folder, if a marker sits beside its executable and the folder can be
/// written. Resolved once: the check creates the folder and writes a probe file.
fn portable_dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = std::env::current_exe().ok()?;
        match portable_dir_in(exe.parent()?) {
            Ok(dir) => dir,
            Err(why) => {
                log::warn!("{why}");
                None
            }
        }
    })
    .clone()
}

/// `<exe_dir>/LightCraftData` when `exe_dir` holds a portable marker; `Err` says why a marked folder
/// can't be used (the caller falls back to the platform folder).
fn portable_dir_in(exe_dir: &Path) -> Result<Option<PathBuf>, String> {
    let Some(marker) = PORTABLE_MARKERS.iter().map(|m| exe_dir.join(m)).find(|p| p.is_file()) else {
        return Ok(None);
    };
    let data = exe_dir.join(PORTABLE_DATA_DIR);
    match ensure_writable(&data) {
        Ok(()) => Ok(Some(data)),
        Err(e) => Err(format!(
            "portable mode ({} found) but {} isn't writable ({e}); settings are kept in the usual folder instead",
            marker.display(),
            data.display()
        )),
    }
}

/// Create `dir` and prove a file can be written in it (a folder can exist yet be read-only).
fn ensure_writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".write-test-{}", std::process::id()));
    std::fs::write(&probe, b"")?;
    // A leftover probe is harmless; don't fail portable mode over it.
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Where face models are kept: `$LIGHTCRAFT_FACE_MODELS` if set, else `<config>/models`.
pub fn default_face_models_dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_FACE_MODELS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("models")))
}

/// Where opt-in denoise models are kept: `$LIGHTCRAFT_DENOISE_MODELS` if set, else `<config>/denoise-models`.
pub fn default_denoise_models_dir() -> Option<PathBuf> {
    std::env::var_os("LIGHTCRAFT_DENOISE_MODELS")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| config_dir().map(|d| d.join("denoise-models")))
}

/// Where the SAM 3 model is kept: `$LIGHTCRAFT_SAM3_DIR` if set, else `<config>/models/sam3`.
pub fn default_sam3_dir() -> Option<PathBuf> {
    sam3_dir(std::env::var_os("LIGHTCRAFT_SAM3_DIR"), config_dir())
}

/// [`default_sam3_dir`] for a given override and config folder: a non-empty override wins.
fn sam3_dir(var: Option<std::ffi::OsString>, config: Option<PathBuf>) -> Option<PathBuf> {
    var.filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| config.map(|d| d.join("models").join("sam3")))
}

impl Session {
    /// Look for the SAM 3 model in the shared default folder ([`default_sam3_dir`]), and for the user's
    /// download mirrors in `<config>/models/sam3-mirrors.txt`.
    pub fn with_default_sam3_model(mut self) -> Self {
        self.segmenter.dir = default_sam3_dir();
        self.segmenter.mirrors_file = config_dir().map(|d| d.join("models").join("sam3-mirrors.txt"));
        self
    }

    /// Keep face models in the shared default folder ([`default_face_models_dir`]).
    pub fn with_default_face_models(mut self) -> Self {
        self.face_models_dir = default_face_models_dir();
        self
    }

    /// Keep denoise models in the shared default folder ([`default_denoise_models_dir`]).
    pub fn with_default_denoise_models(mut self) -> Self {
        self.set_denoise_models_dir(default_denoise_models_dir());
        self
    }

    /// Point AI denoise at `dir` (or at no folder), dropping what was learned about the old one.
    pub fn set_denoise_models_dir(&mut self, dir: Option<PathBuf>) {
        self.denoise.models_dir = dir;
        self.denoise.touch();
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn resolve(platform: Platform, vars: &[(&str, &str)]) -> Option<PathBuf> {
        config_dir_with(platform, |name| vars.iter().find_map(|(key, value)| (*key == name).then(|| OsString::from(value))), || None)
    }

    /// An override is used as is: portable mode is not even looked for.
    fn no_portable_probe() -> Option<PathBuf> {
        panic!("an override must not probe for portable mode")
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A marker beside the executable moves the settings folder next to it, ahead of the platform folder (issue #1200).
    #[test]
    fn portable_marker_puts_settings_beside_the_executable() {
        for marker in PORTABLE_MARKERS {
            let exe = temp(&format!("portable-{marker}"));
            assert_eq!(portable_dir_in(&exe), Ok(None), "no marker, no portable folder");
            assert!(!exe.join(PORTABLE_DATA_DIR).exists());
            std::fs::write(exe.join(marker), b"").unwrap();
            let data = exe.join(PORTABLE_DATA_DIR);
            assert_eq!(portable_dir_in(&exe), Ok(Some(data.clone())));
            assert!(data.is_dir());
            assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0, "the write probe is removed");
            let found = portable_dir_in(&exe).unwrap();
            let actual =
                config_dir_with(Platform::Windows, |name| (name == "APPDATA").then(|| OsString::from(r"C:\Users\me\AppData\Roaming")), || found);
            assert_eq!(actual, Some(data));
            let _ = std::fs::remove_dir_all(&exe);
        }
    }

    /// A marked folder that can't hold the data folder (here: a file is in its place) is reported, not used.
    #[test]
    fn portable_folder_that_cannot_be_written_is_refused() {
        let exe = temp("portable-blocked");
        std::fs::write(exe.join("portable.txt"), b"").unwrap();
        std::fs::write(exe.join(PORTABLE_DATA_DIR), b"").unwrap();
        let why = portable_dir_in(&exe).unwrap_err();
        assert!(why.contains("portable.txt") && why.contains(PORTABLE_DATA_DIR), "{why}");
        let _ = std::fs::remove_dir_all(&exe);
    }

    #[test]
    fn config_override_wins_without_reading_user_profile() {
        for platform in [Platform::Mac, Platform::Windows, Platform::Other] {
            let actual = config_dir_with(
                platform,
                |name| {
                    assert_eq!(name, "LIGHTCRAFT_CONFIG_DIR");
                    Some(OsString::from("isolated/settings"))
                },
                no_portable_probe,
            );
            assert_eq!(actual, Some(PathBuf::from("isolated/settings")));
        }
    }

    #[test]
    fn config_empty_override_disables_profile_without_reading_defaults() {
        for platform in [Platform::Mac, Platform::Windows, Platform::Other] {
            assert_eq!(
                config_dir_with(
                    platform,
                    |name| {
                        assert_eq!(name, "LIGHTCRAFT_CONFIG_DIR");
                        Some(OsString::new())
                    },
                    no_portable_probe
                ),
                None
            );
        }
    }

    #[test]
    fn config_unset_override_keeps_platform_defaults() {
        let vars = [("HOME", "/user"), ("APPDATA", r"C:\Users\me\AppData\Roaming"), ("XDG_CONFIG_HOME", "/xdg")];
        assert_eq!(resolve(Platform::Mac, &vars), Some(PathBuf::from("/user/Library/Application Support/LightCraft")));
        assert_eq!(resolve(Platform::Windows, &vars), Some(PathBuf::from(r"C:\Users\me\AppData\Roaming").join("LightCraft")));
        assert_eq!(resolve(Platform::Other, &vars), Some(PathBuf::from("/xdg/lightcraft")));
        assert_eq!(resolve(Platform::Other, &[("HOME", "/user")]), Some(PathBuf::from("/user/.config/lightcraft")));
        for platform in [Platform::Mac, Platform::Windows, Platform::Other] {
            assert_eq!(resolve(platform, &[]), None);
        }
    }

    /// #2572 review: an empty or relative `HOME`, `APPDATA` or `XDG_CONFIG_HOME` would put the settings
    /// folder (and the crash log and logs in it) under the launch directory, where another user could
    /// have planted the folder and a link. Such a value is ignored: an unusable `XDG_CONFIG_HOME` falls
    /// back to `HOME`, and with neither usable there is no settings folder.
    #[test]
    fn config_ignores_empty_or_relative_platform_folders() {
        for bad in ["", "relative", "./here", "../up"] {
            assert_eq!(resolve(Platform::Mac, &[("HOME", bad)]), None, "HOME={bad:?}");
            assert_eq!(resolve(Platform::Other, &[("HOME", bad)]), None, "HOME={bad:?}");
            assert_eq!(resolve(Platform::Windows, &[("APPDATA", bad), ("HOME", "/user")]), None, "APPDATA={bad:?}");
            assert_eq!(resolve(Platform::Other, &[("XDG_CONFIG_HOME", bad)]), None, "XDG_CONFIG_HOME={bad:?}");
            assert_eq!(resolve(Platform::Other, &[("XDG_CONFIG_HOME", bad), ("HOME", bad)]), None, "both {bad:?}");
            assert_eq!(
                resolve(Platform::Other, &[("XDG_CONFIG_HOME", bad), ("HOME", "/user")]),
                Some(PathBuf::from("/user/.config/lightcraft")),
                "XDG_CONFIG_HOME={bad:?} falls back to HOME"
            );
            assert_eq!(
                resolve(Platform::Other, &[("XDG_CONFIG_HOME", "/xdg"), ("HOME", bad)]),
                Some(PathBuf::from("/xdg/lightcraft")),
                "a usable XDG_CONFIG_HOME doesn't need HOME"
            );
        }
    }

    /// #2572 review: on Windows `APPDATA` must be fully qualified. `\foo` and `/foo` follow the current
    /// drive and `C:foo` that drive's current folder, so the launch drive could choose the folder.
    #[test]
    fn windows_appdata_must_be_fully_qualified() {
        for good in [r"C:\Users\me\AppData\Roaming", "D:/Data", r"\\server\share\Roaming", r"\\?\C:\Users\me\AppData\Roaming"] {
            assert_eq!(resolve(Platform::Windows, &[("APPDATA", good)]), Some(PathBuf::from(good).join("LightCraft")), "APPDATA={good:?}");
        }
        for bad in [r"\foo", "/roaming", "C:foo", "C:", "c", r"\\"] {
            assert_eq!(resolve(Platform::Windows, &[("APPDATA", bad)]), None, "APPDATA={bad:?}");
        }
    }

    /// The explicit profile keeps its own policy: a relative `LIGHTCRAFT_CONFIG_DIR` is the user's choice.
    #[test]
    fn config_override_may_still_be_relative() {
        for platform in [Platform::Mac, Platform::Windows, Platform::Other] {
            assert_eq!(resolve(platform, &[("LIGHTCRAFT_CONFIG_DIR", "isolated"), ("HOME", "")]), Some(PathBuf::from("isolated")));
        }
    }

    #[cfg(unix)]
    #[test]
    fn config_override_preserves_non_unicode_paths() {
        use std::os::unix::ffi::OsStringExt as _;
        let value = OsString::from_vec(b"isolated/\xff".to_vec());
        assert_eq!(config_dir_with(Platform::Mac, |_| Some(value.clone()), no_portable_probe), Some(PathBuf::from(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-empty `LIGHTCRAFT_SAM3_DIR` wins; unset or empty falls back to `<config>/models/sam3` (issue #619).
    #[test]
    fn sam3_dir_prefers_the_override() {
        let config = PathBuf::from("cfg");
        let fallback = Some(config.join("models").join("sam3"));
        assert_eq!(sam3_dir(Some("elsewhere".into()), Some(config.clone())), Some(PathBuf::from("elsewhere")));
        assert_eq!(sam3_dir(None, Some(config.clone())), fallback);
        assert_eq!(sam3_dir(Some(std::ffi::OsString::new()), Some(config)), fallback);
        assert_eq!(sam3_dir(None, None), None);
    }
}
