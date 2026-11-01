//! The AI mask models as the session sees them ([`lightcraft_segment::models`] in builds with the
//! `sam` feature): where each one lives, whether it is installed, and its download (`ai.models.*`).
//!
//! The openly licensed models (SAM 2.1, EoMT, BiRefNet, Depth Anything V2 Small) live in
//! `<models folder>/<dir_name>/`; SAM 3 keeps its own folder ([`Segmenter::dir`]). Nothing is
//! downloaded unless asked for (`ai.models.download {"id", "acknowledged": true}`), each download
//! is a row in the activity stack, can be cancelled, resumes, and every file is checked against its
//! pinned size and SHA-256 before it is used.

use std::path::PathBuf;

use super::{DownloadStatus, Segmenter};

/// How the error about a missing openly licensed model reads (after its title); the UI offers its download on it.
pub const MODEL_NOT_INSTALLED: &str = "model is not installed";

/// One model, for `ai.models.status` and the download dialog.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    /// The command key (`"people"`, `"face"`, `"subject"`, `"scene"`, `"sam2"`, `"sam3"`).
    pub id: String,
    pub title: String,
    /// What it is for.
    pub purpose: String,
    pub installed: bool,
    pub dir: Option<String>,
    pub size_bytes: u64,
    pub license: String,
    pub license_url: String,
    pub page_url: String,
    /// Permissively licensed and ungated (LightCraft may host it).
    pub open: bool,
    /// Whether its training data's licence is a maintainer decision still to make (shown as
    /// "licence pending a maintainer decision"; docs/ai-masks.md).
    pub licence_pending: bool,
    /// Download locations configured (built-in and the user's).
    pub mirrors: usize,
    pub download: DownloadStatus,
}

/// The model keys, in display order (empty without the `sam` feature).
pub fn keys() -> Vec<&'static str> {
    #[cfg(feature = "sam")]
    return lightcraft_segment::models::MODELS.iter().map(|m| m.key).collect();
    #[cfg(not(feature = "sam"))]
    Vec::new()
}

impl Segmenter {
    /// Where model `key` lives (`None`: unknown model, or no models folder).
    pub fn model_dir_for(&self, key: &str) -> Option<PathBuf> {
        if key == "sam3" {
            return self.dir.clone();
        }
        #[cfg(feature = "sam")]
        {
            let m = lightcraft_segment::models::by_key(key)?;
            self.models_root.as_ref().map(|r| r.join(m.dir_name))
        }
        #[cfg(not(feature = "sam"))]
        None
    }

    /// Whether model `key` is installed.
    pub fn model_installed(&self, key: &str) -> bool {
        if key == "sam3" {
            return self.installed();
        }
        #[cfg(feature = "sam")]
        if let (Some(m), Some(dir)) = (lightcraft_segment::models::by_key(key), self.model_dir_for(key)) {
            return m.installed_in(&dir);
        }
        false
    }

    /// Where model `key` is downloaded from, in order (the user's mirrors first).
    pub fn model_mirrors(&self, key: &str) -> Vec<String> {
        if key == "sam3" {
            return self.mirrors();
        }
        #[cfg(feature = "sam")]
        if let Some(m) = lightcraft_segment::models::by_key(key) {
            let env = std::env::var(m.mirrors_env).ok();
            let file = self.models_root.as_ref().map(|r| r.join(m.mirrors_file_name()));
            return m.mirrors(env.as_deref(), file.as_deref());
        }
        Vec::new()
    }

    /// The download of model `key`.
    pub fn model_download_status(&self, key: &str) -> DownloadStatus {
        if key == "sam3" {
            return self.download_status();
        }
        #[cfg(feature = "sam")]
        if let Some(d) = self.model_downloads.get(key) {
            let s = d.status();
            return DownloadStatus {
                running: s.running,
                done: s.done,
                total: s.total,
                file: s.file,
                error: s.error,
                cancelled: s.cancelled,
                finished: s.finished,
            };
        }
        DownloadStatus::default()
    }

    /// Every model this build knows, with its state.
    pub fn model_status(&self) -> Vec<ModelStatus> {
        #[cfg(feature = "sam")]
        {
            lightcraft_segment::models::MODELS
                .iter()
                .map(|m| ModelStatus {
                    id: m.key.to_string(),
                    title: m.title.to_string(),
                    purpose: m.purpose.to_string(),
                    installed: self.model_installed(m.key),
                    dir: self.model_dir_for(m.key).map(|d| d.display().to_string()),
                    size_bytes: m.size(),
                    license: m.license_name.to_string(),
                    license_url: m.license_url.to_string(),
                    page_url: m.page_url.to_string(),
                    open: m.open,
                    licence_pending: m.licence_pending,
                    mirrors: self.model_mirrors(m.key).len(),
                    download: self.model_download_status(m.key),
                })
                .collect()
        }
        #[cfg(not(feature = "sam"))]
        Vec::new()
    }

    /// The folder of model `key` when it is installed; otherwise an error that says how to get it
    /// (it contains [`MODEL_NOT_INSTALLED`]).
    pub fn require_model(&self, key: &str) -> Result<PathBuf, String> {
        if !Self::AVAILABLE {
            return Err("AI masks are not available in this build".into());
        }
        if key == "sam3" {
            return self.model_dir();
        }
        #[cfg(feature = "sam")]
        {
            let m = lightcraft_segment::models::by_key(key).ok_or_else(|| format!("unknown AI model `{key}`"))?;
            let dir = self.model_dir_for(key).ok_or("there is no folder for AI models in this session")?;
            if m.installed_in(&dir) {
                return Ok(dir);
            }
            let d = self.model_download_status(key);
            if d.running {
                let pct = d.done.saturating_mul(100).checked_div(d.total).unwrap_or(0);
                return Err(format!("The {} {MODEL_NOT_INSTALLED} yet: it is downloading ({pct} %).", m.title));
            }
            Err(format!(
                "The {} {MODEL_NOT_INSTALLED} ({}). Download it once (about {:.0} MB, {} licence) with `ai.models.download {{\"id\": \"{}\", \"acknowledged\": true}}` or from the Masking panel, or put {} in {}.",
                m.title,
                m.purpose,
                m.size() as f64 / 1e6,
                m.license_name,
                m.key,
                m.files.iter().map(|f| f.name).collect::<Vec<_>>().join(", "),
                dir.display()
            ))
        }
        #[cfg(not(feature = "sam"))]
        Err(format!("unknown AI model `{key}`"))
    }

    /// Start downloading model `key` in the background (a row in the activity stack). `Ok(false)`
    /// when it is installed or downloading already.
    pub fn start_model_download(&mut self, key: &str, activity: &crate::activity::Activity) -> Result<bool, String> {
        if !Self::AVAILABLE {
            return Err("AI masks are not available in this build".into());
        }
        if key == "sam3" {
            return self.start_download(activity);
        }
        #[cfg(feature = "sam")]
        {
            let m = lightcraft_segment::models::by_key(key).ok_or_else(|| format!("unknown AI model `{key}`"))?;
            let dir = self.model_dir_for(key).ok_or("there is no folder for AI models in this session")?;
            if m.installed_in(&dir) {
                return Ok(false);
            }
            let mirrors = self.model_mirrors(key);
            if mirrors.is_empty() {
                return Err(format!("no download location is configured for the {} model (set {})", m.title, m.mirrors_env));
            }
            self.model_downloads.entry(m.key).or_default().start(m.files, mirrors, dir, m.options(), activity, m.title)
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = activity;
            Err(format!("unknown AI model `{key}`"))
        }
    }

    /// Stop model `key`'s download (it resumes next time). False when none runs.
    pub fn cancel_model_download(&self, key: &str) -> bool {
        if key == "sam3" {
            return self.cancel_download();
        }
        #[cfg(feature = "sam")]
        return self.model_downloads.get(key).is_some_and(|d| d.cancel());
        #[cfg(not(feature = "sam"))]
        false
    }
}

/// A segmentation error for a command: a missing openly licensed model is typed
/// ([`crate::ErrorKind::ModelMissing`] with [`crate::AiModel::Open`]), so a frontend offers it
/// without reading the words; anything else is [`crate::EngineError::Other`].
pub fn segment_error(msg: String) -> crate::EngineError {
    match missing_model(&msg) {
        Some(key) => crate::EngineError::ModelMissing { model: crate::AiModel::Open(key), msg },
        None => crate::EngineError::Other(msg),
    }
}

/// The key of the openly licensed model an error from [`Segmenter::require_model`] says is missing (the UI
/// then offers its download), if it is one.
pub fn missing_model(error: &str) -> Option<&'static str> {
    #[cfg(feature = "sam")]
    return lightcraft_segment::models::MODELS
        .iter()
        .filter(|m| m.open)
        .find(|m| error.starts_with(&format!("The {} {MODEL_NOT_INSTALLED}", m.title)))
        .map(|m| m.key);
    #[cfg(not(feature = "sam"))]
    {
        let _ = error;
        None
    }
}
