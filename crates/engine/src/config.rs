//! Where the app keeps per-user data on this machine: the config folder (app settings, camera
//! profiles, face and denoise models).
//!
//! [`config_dir`] picks it, in order:
//! 1. `$<PREFIX>_CONFIG_DIR`, when set (tests, agents, custom setups).
//! 2. **Portable mode**: a marker file ([`portable_markers`]) beside the executable puts everything in
//!    `<exe dir>/<settings dir>Data` ([`portable_data_dir`]), so a copy on a USB stick leaves nothing in the
//!    user profile. If that folder can't be written (read-only medium, Program Files), a warning is logged and
//!    the platform folder is used.
//! 3. The platform convention ([`lcc_brand::settings_dir`]).
//!
//! Hosts that have a file system (the desktop app, the CLI, the MCP server) share these defaults so a model
//! installed from one is there for the others. Nothing here is applied automatically: a [`Session`] has no
//! face-, denoise- or SAM 3 model folder until a host asks for one ([`Session::with_default_face_models`],
//! [`Session::with_default_denoise_models`], [`Session::with_default_sam3_model`]), so tests stay hermetic.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::Session;

/// Files beside the executable that switch on portable mode (either one; contents are ignored):
/// `portable.txt` and `<binary>.portable`.
pub fn portable_markers() -> [String; 2] {
    ["portable.txt".to_owned(), format!("{}.portable", lcc_brand::BINARY)]
}

/// The data folder created beside the executable in portable mode: `<settings dir name>Data`, without spaces.
pub fn portable_data_dir() -> String {
    format!("{}Data", lcc_brand::SETTINGS_DIR.replace(' ', ""))
}

/// The app's settings folder ([`lcc_brand::settings_dir`]). A nonempty `$<PREFIX>_CONFIG_DIR` overrides
/// portable mode and the platform default. The override is the complete directory; no application-name suffix
/// is appended. An explicitly empty value disables the settings directory instead of falling back to a personal
/// profile.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_with(lcc_brand::env_os("CONFIG_DIR"), portable_dir, lcc_brand::settings_dir)
}

/// The recent-catalogs list (File ▸ Open Recent Catalog, the startup default) in [`config_dir`], like every
/// other settings file: `<PREFIX>_CONFIG_DIR` and portable mode apply; an empty override keeps no list.
#[cfg(not(target_arch = "wasm32"))]
pub fn recent_catalogs_file() -> Option<PathBuf> {
    config_dir().map(|d| crate::catalog::library::RecentCatalogs::file_in(&d))
}

/// `portable` is asked only without an override, so an isolated run never probes beside the executable.
fn config_dir_with(over: Option<OsString>, portable: impl FnOnce() -> Option<PathBuf>, default: impl FnOnce() -> Option<PathBuf>) -> Option<PathBuf> {
    match over {
        Some(dir) => (!dir.is_empty()).then(|| PathBuf::from(dir)),
        None => portable().or_else(default),
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

/// `<exe_dir>/<portable data dir>` when `exe_dir` holds a portable marker; `Err` says why a marked folder
/// can't be used (the caller falls back to the platform folder).
fn portable_dir_in(exe_dir: &Path) -> Result<Option<PathBuf>, String> {
    let Some(marker) = portable_markers().iter().map(|m| exe_dir.join(m)).find(|p| p.is_file()) else {
        return Ok(None);
    };
    let data = exe_dir.join(portable_data_dir());
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

/// Where face models are kept: `{ENV_PREFIX}_FACE_MODELS` if set, else `<config>/models`.
pub fn default_face_models_dir() -> Option<PathBuf> {
    lcc_brand::env_os("FACE_MODELS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("models")))
}

/// Where opt-in denoise models are kept: `{ENV_PREFIX}_DENOISE_MODELS` if set, else `<config>/denoise-models`.
pub fn default_denoise_models_dir() -> Option<PathBuf> {
    lcc_brand::env_os("DENOISE_MODELS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("denoise-models")))
}

/// Where the SAM 3 model is kept: `$<PREFIX>_SAM3_DIR` if set, else `<config>/models/sam3`.
pub fn default_sam3_dir() -> Option<PathBuf> {
    sam3_dir(lcc_brand::env_os("SAM3_DIR"), config_dir())
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
        // the Subject / Background model: `$<PREFIX>_BIREFNET_DIR`, else `<config>/models/birefnet`
        self.segmenter.subject.dir = lcc_brand::env_os("BIREFNET_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| config_dir().map(|d| d.join("models").join("birefnet")));
        self.segmenter.subject.sync(&self.activity);
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

    /// Keep the lens database setting in the shared settings folder ([`config_dir`]) and use the folder saved there.
    pub fn with_default_lens_profiles(mut self) -> Self {
        self.lens_db = crate::lens_download::LensSettings::load(config_dir());
        self
    }

    /// Whether photo `id`'s lens data predates the lens database in use (it changed since the photo was looked up).
    pub fn lens_profiles_stale(&self, id: lcc_catalog::PhotoId) -> bool {
        let g = crate::lens_profiles::generation();
        g != 0 && self.lens_seen.get(&id) != Some(&g)
    }

    /// Look the lenses of raw photos `ids` up again in the lens database in use (after it changed), so their lens
    /// corrections and previews follow. → the photos whose lens data changed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn refresh_lens_profiles(&mut self, ids: &[lcc_catalog::PhotoId]) -> crate::Result<Vec<lcc_catalog::PhotoId>> {
        let g = crate::lens_profiles::generation();
        let files: Vec<(lcc_catalog::PhotoId, String)> = ids
            .iter()
            .filter_map(|id| {
                self.lens_seen.insert(*id, g);
                let p = self.catalog.photo(*id)?;
                match &p.source {
                    lcc_catalog::Source::File { path } if p.develops_raw() => Some((*id, path.clone())),
                    _ => None,
                }
            })
            .collect();
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let probed = crate::import::probe_paths(self, &files.iter().map(|(_, p)| p.clone()).collect::<Vec<_>>());
        let mut ops = Vec::new();
        let mut changed = Vec::new();
        for ((id, _), info) in files.into_iter().zip(probed) {
            let (Ok(info), Some(ph)) = (info, self.catalog.photo(id)) else { continue };
            self.lens_from_file.insert(id, info.lens_from_file);
            let mine = crate::cmd::convert::lens_ops(id, ph, &info);
            if !mine.is_empty() {
                ops.extend(mine);
                changed.push(id);
            }
        }
        for id in &changed {
            self.media.forget(*id);
        }
        if !ops.is_empty() {
            // found by the app, not done by the user: saved, but no undo step
            self.apply_unrecorded(lcc_catalog::Op::Batch { ops })?;
        }
        Ok(changed)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn refresh_lens_profiles(&mut self, ids: &[lcc_catalog::PhotoId]) -> crate::Result<Vec<lcc_catalog::PhotoId>> {
        let g = crate::lens_profiles::generation();
        for id in ids {
            self.lens_seen.insert(*id, g);
        }
        Ok(Vec::new())
    }

    /// What the lens database says about `p`'s lens, for Develop ▸ Optics; `None` when the photo carries a lens
    /// correction already (its own, or one found earlier).
    pub fn lens_match(&self, p: &lcc_catalog::Photo) -> Option<crate::lens_profiles::Match> {
        self.lens_match_with(p, None)
    }

    /// [`Session::lens_match`] for a lens given by the caller (a lens picker's choice) instead of the one the
    /// metadata names, in the order corrections apply ([`crate::lens_profiles::resolve_raw`]): `None` when the
    /// file carries its own correction, else [`crate::lens_profiles::Match::Chosen`] (or `NotFound`) for the given
    /// lens, else the match by name. A correction found earlier by name gives way to a given lens.
    pub fn lens_match_with(&self, p: &lcc_catalog::Photo, given: Option<&crate::lens_profiles::GivenLens>) -> Option<crate::lens_profiles::Match> {
        use crate::lens_profiles::Match;
        if !p.develops_raw() {
            return if p.embedded_lens.is_some() { None } else { Some(Match::NotRaw) };
        }
        if p.embedded_lens.is_some() {
            // the file's own, as this session recorded when it looked the photo up; unrecorded, the file's unless
            // the database matches its lens name (only then could the correction have come from there)
            let from_file = self
                .lens_from_file
                .get(&p.id)
                .copied()
                .unwrap_or_else(|| !matches!(crate::lens_profiles::match_lens(None, &p.meta.lens), Match::Found(_)));
            if from_file {
                return None;
            }
        }
        Some(crate::lens_profiles::match_lens_with(None, &p.meta.lens, given))
    }

    /// Point AI denoise at `dir` (or at no folder), dropping what was learned about the old one.
    pub fn set_denoise_models_dir(&mut self, dir: Option<PathBuf>) {
        self.denoise.models_dir = dir;
        self.denoise.touch();
    }
}

#[cfg(target_arch = "wasm32")]
impl Session {
    /// Remote accounts are native only: nothing to set up in the browser.
    pub fn with_default_connections(self) -> Self {
        self
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lcc-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A marker beside the executable moves the settings folder next to it, ahead of the platform folder.
    #[test]
    fn portable_marker_puts_settings_beside_the_executable() {
        for (i, marker) in portable_markers().iter().enumerate() {
            let exe = temp(&format!("portable-{i}"));
            assert_eq!(portable_dir_in(&exe), Ok(None), "no marker, no portable folder");
            assert!(!exe.join(portable_data_dir()).exists());
            std::fs::write(exe.join(marker), b"").unwrap();
            let data = exe.join(portable_data_dir());
            assert_eq!(portable_dir_in(&exe), Ok(Some(data.clone())));
            assert!(data.is_dir());
            assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0, "the write probe is removed");
            let found = portable_dir_in(&exe).unwrap();
            assert_eq!(config_dir_with(None, || found, || Some(PathBuf::from("/roaming"))), Some(data));
            let _ = std::fs::remove_dir_all(&exe);
        }
    }

    /// A marked folder that can't hold the data folder (here: a file is in its place) is reported, not used.
    #[test]
    fn portable_folder_that_cannot_be_written_is_refused() {
        let exe = temp("portable-blocked");
        std::fs::write(exe.join("portable.txt"), b"").unwrap();
        std::fs::write(exe.join(portable_data_dir()), b"").unwrap();
        let why = portable_dir_in(&exe).unwrap_err();
        assert!(why.contains("portable.txt") && why.contains(&portable_data_dir()), "{why}");
        let _ = std::fs::remove_dir_all(&exe);
    }

    #[test]
    fn config_override_wins_without_reading_user_profile() {
        let actual = config_dir_with(Some(OsString::from("isolated/settings")), || panic!("no portable probe"), || panic!("the default is not read"));
        assert_eq!(actual, Some(PathBuf::from("isolated/settings")));
    }

    #[test]
    fn config_empty_override_disables_profile_without_reading_defaults() {
        assert_eq!(config_dir_with(Some(OsString::new()), || panic!("no portable probe"), || panic!("the default is not read")), None);
    }

    #[test]
    fn config_unset_override_keeps_platform_defaults() {
        assert_eq!(config_dir_with(None, || None, || Some(PathBuf::from("/xdg/app"))), Some(PathBuf::from("/xdg/app")));
        assert_eq!(config_dir_with(None, || None, || None), None);
    }

    #[cfg(unix)]
    #[test]
    fn config_override_preserves_non_unicode_paths() {
        use std::os::unix::ffi::OsStringExt as _;
        let value = OsString::from_vec(b"isolated/\xff".to_vec());
        assert_eq!(config_dir_with(Some(value.clone()), || None, || None), Some(PathBuf::from(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #163: the recent-catalogs list follows the config-dir resolver, never the platform folder
    /// when `<PREFIX>_CONFIG_DIR` is set, and none at all when it is empty.
    #[test]
    fn recent_catalogs_follow_the_config_dir() {
        use crate::catalog::library::RecentCatalogs;
        let tmp = std::env::temp_dir().join(format!("lcc-recent-163-{}", std::process::id()));
        let (scratch, platform) = (tmp.join("scratch"), tmp.join("platform"));
        let platform_file = RecentCatalogs::file_in(&platform);
        let real = RecentCatalogs { default: Some(platform.join("real-lib")), ..Default::default() };
        real.save(&platform_file).unwrap();
        let resolve = |over: Option<OsString>| config_dir_with(over, || None, || Some(platform.clone())).map(|d| RecentCatalogs::file_in(&d));
        // AC-1/AC-2: with an override, the list lives in the override and the platform default isn't read.
        let file = resolve(Some(scratch.clone().into_os_string())).unwrap();
        assert!(file.starts_with(&scratch));
        assert_eq!(RecentCatalogs::load(&file).default, None);
        RecentCatalogs { paths: vec![scratch.join("lib")], ..Default::default() }.save(&file).unwrap();
        assert!(file.exists());
        assert_eq!(RecentCatalogs::load(&platform_file).default, real.default);
        // AC-3: empty override, no list.
        assert_eq!(resolve(Some(OsString::new())), None);
        // AC-4: portable mode.
        let portable = tmp.join("portable");
        let p = config_dir_with(None, || Some(portable.clone()), || Some(platform.clone())).unwrap();
        assert!(RecentCatalogs::file_in(&p).starts_with(&portable));
        // AC-5: no override, the platform folder as before.
        assert_eq!(resolve(None), Some(platform_file));
        // AC-6: the public resolver agrees with config_dir.
        assert_eq!(recent_catalogs_file(), config_dir().map(|d| RecentCatalogs::file_in(&d)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A non-empty `<PREFIX>_SAM3_DIR` wins; unset or empty falls back to `<config>/models/sam3` (issue #619).
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
