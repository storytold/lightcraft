//! Where LightCraft keeps per-user data on this machine: the config folder (app settings, camera
//! profiles, face and denoise models).
//!
//! Hosts that have a file system (the desktop app, the CLI, the MCP server) share these defaults so a model
//! installed from one is there for the others. Nothing here is applied automatically: a [`Session`] has no
//! face-, denoise- or SAM 3 model folder until a host asks for one ([`Session::with_default_face_models`],
//! [`Session::with_default_denoise_models`], [`Session::with_default_sam3_model`]), so tests stay hermetic.

use std::path::PathBuf;

use crate::Session;

/// LightCraft's per-user config folder (`…/LightCraft`), if the platform tells us where.
pub fn config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/LightCraft"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("LightCraft"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|c| c.join("lightcraft"))
    }
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
