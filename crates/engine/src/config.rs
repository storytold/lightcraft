//! Per-user data folders: see [`dac_engine_core::config`]; the session-side half lives here.
use std::path::PathBuf;

pub use dac_engine_core::config::*;

use crate::Session;

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
