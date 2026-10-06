//! Where LightCraft keeps per-user data on this machine: the config folder (app settings, face models).
//!
//! Hosts that have a file system (the desktop app, the CLI, the MCP server) share these defaults so a model
//! installed from one is there for the others. Nothing here is applied automatically: a [`Session`] has no
//! face-models folder until a host asks for one ([`Session::with_default_face_models`]), so tests stay
//! hermetic.

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

impl Session {
    /// Keep face models in the shared default folder ([`default_face_models_dir`]).
    pub fn with_default_face_models(mut self) -> Self {
        self.face_models_dir = default_face_models_dir();
        self
    }
}
