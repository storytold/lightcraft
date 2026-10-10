//! Where the app keeps per-user data on this machine: the config folder (app settings, camera
//! profiles, face and denoise models).
//!
//! Hosts that have a file system (the desktop app, the CLI, the MCP server) share these defaults so a model
//! installed from one is there for the others. Nothing here is applied automatically: a [`Session`] has no
//! face- or denoise-models folder until a host asks for one ([`Session::with_default_face_models`],
//! [`Session::with_default_denoise_models`]), so tests stay hermetic.

use std::path::PathBuf;

use crate::Session;

/// The app's per-user config folder ([`dac_brand::settings_dir`]), if the platform tells us where.
pub fn config_dir() -> Option<PathBuf> {
    dac_brand::settings_dir()
}

/// Where face models are kept: `{ENV_PREFIX}_FACE_MODELS` if set, else `<config>/models`.
pub fn default_face_models_dir() -> Option<PathBuf> {
    dac_brand::env_os("FACE_MODELS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("models")))
}

/// Where opt-in denoise models are kept: `{ENV_PREFIX}_DENOISE_MODELS` if set, else `<config>/denoise-models`.
pub fn default_denoise_models_dir() -> Option<PathBuf> {
    dac_brand::env_os("DENOISE_MODELS").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| config_dir().map(|d| d.join("denoise-models")))
}

impl Session {
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

#[cfg(target_arch = "wasm32")]
impl Session {
    /// Remote accounts are native only: nothing to set up in the browser.
    pub fn with_default_connections(self) -> Self {
        self
    }
}
