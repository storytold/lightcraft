//! The Subject (and Background) AI mask model: BiRefNet_lite (`lcc_segment::birefnet`, MIT
//! code and weights).
//!
//! Downloaded only when asked for (`segment.subject.download`, 178 MB, pinned by size and
//! SHA-256, from the author's Hugging Face repository). Once its folder holds the weights, the
//! model is installed as the pipeline's subject model ([`lcc_pipeline::auto_mask::set_subject_model`]):
//! Subject and Background masks then use its selection, computed once per photo (cached) and shown
//! in the activity stack while it runs. Without it they keep the photo analysis of
//! `auto_mask.rs`. A failure to load or run the model is logged and falls back the same way.

use std::path::{Path, PathBuf};

use crate::activity::Activity;

/// Download size of the model.
pub const MODEL_BYTES: u64 = 177_634_392;
pub const LICENSE_NAME: &str = "MIT";
pub const LICENSE_URL: &str = "https://huggingface.co/ZhengPeng7/BiRefNet_lite";

/// The subject model's folder and download.
#[derive(Default)]
pub struct Subject {
    /// Where `model.safetensors` lives (`None`: no model).
    pub dir: Option<PathBuf>,
    #[cfg(feature = "sam")]
    download: super::download::Downloader,
    /// The folder the installed pipeline model was loaded from.
    installed_from: Option<PathBuf>,
}

impl Subject {
    /// Whether the weights are in place.
    pub fn installed(&self) -> bool {
        #[cfg(feature = "sam")]
        if let Some(d) = &self.dir {
            return lcc_segment::birefnet::is_model_dir(d);
        }
        false
    }

    /// Whether the pipeline uses the model now.
    pub fn active(&self) -> bool {
        self.installed_from.is_some() && lcc_pipeline::auto_mask::has_subject_model()
    }

    /// Install the model for the pipeline when its weights are present (idempotent), or remove
    /// it when they aren't. Inference runs hold an activity row (`segment`, "Finding the subject").
    pub fn sync(&mut self, activity: &Activity) {
        let want = self.installed().then(|| self.dir.clone()).flatten();
        if want == self.installed_from {
            return;
        }
        match &want {
            Some(dir) => install(dir, activity.clone()),
            None => lcc_pipeline::auto_mask::set_subject_model(None),
        }
        self.installed_from = want;
    }

    pub fn download_status(&self) -> super::DownloadStatus {
        #[cfg(feature = "sam")]
        {
            let s = self.download.status();
            super::DownloadStatus {
                running: s.running,
                done: s.done,
                total: s.total,
                file: s.file,
                error: s.error,
                finished: s.finished,
                cancelled: s.cancelled,
            }
        }
        #[cfg(not(feature = "sam"))]
        super::DownloadStatus::default()
    }

    /// Start the download in the background. `Ok(false)` when installed or already running.
    pub fn start_download(&self, activity: &Activity) -> Result<bool, String> {
        if !super::Segmenter::AVAILABLE {
            return Err("AI masks are not available in this build".into());
        }
        let dir = self.dir.clone().ok_or("no folder is set for the subject model")?;
        if self.installed() {
            return Ok(false);
        }
        #[cfg(feature = "sam")]
        {
            use lcc_segment::fetch;
            let env = lcc_brand::env(fetch::BIREFNET_MIRRORS_ENV);
            let mirrors = fetch::birefnet_mirrors(env.as_deref());
            self.download.start(fetch::BIREFNET_FILES, mirrors, dir, fetch::Options::default(), activity, "BiRefNet")
        }
        #[cfg(not(feature = "sam"))]
        {
            let _ = (dir, activity);
            Ok(false)
        }
    }

    pub fn cancel_download(&self) -> bool {
        #[cfg(feature = "sam")]
        return self.download.cancel();
        #[cfg(not(feature = "sam"))]
        false
    }
}

/// Install the model in `dir` as the pipeline's subject model (loaded on first use).
#[cfg(feature = "sam")]
fn install(dir: &Path, activity: Activity) {
    use std::sync::{Arc, Mutex, PoisonError};

    use lcc_segment::birefnet::BiRefNet;

    let dir = dir.to_path_buf();
    let model: Mutex<Option<BiRefNet>> = Mutex::new(None);
    let run = move |rgb: &[u8], w: usize, h: usize| -> Option<lcc_raster::Plane> {
        let _task = activity.start("segment", "Finding the subject", crate::activity::Cancel::No);
        let mut m = model.lock().unwrap_or_else(PoisonError::into_inner);
        if m.is_none() {
            match BiRefNet::load(&dir).or_else(|e| {
                log::warn!("subject model on the GPU: {e}; using the CPU");
                BiRefNet::load_cpu(&dir)
            }) {
                Ok(b) => *m = Some(b),
                Err(e) => {
                    log::warn!("subject model: {e}");
                    return None;
                }
            }
        }
        let started = std::time::Instant::now();
        let mut result = m.as_ref()?.predict(rgb, w, h);
        // a GPU (Metal) run that fails — an op the device lacks, a virtual GPU without room —
        // falls back to the CPU, which then keeps the model
        if let Err(e) = &result
            && !m.as_ref().is_some_and(BiRefNet::on_cpu)
        {
            log::warn!("subject model on the GPU: {e}; using the CPU");
            match BiRefNet::load_cpu(&dir) {
                Ok(b) => {
                    result = b.predict(rgb, w, h);
                    *m = Some(b);
                }
                Err(e) => log::warn!("subject model: {e}"),
            }
        }
        let matte = match result {
            Ok(p) => p,
            Err(e) => {
                log::warn!("subject model: {e}");
                return None;
            }
        };
        log::info!("subject model: {w}×{h} in {:?}", started.elapsed());
        Some(lcc_raster::Plane { width: matte.side, height: matte.side, data: matte.data })
    };
    lcc_pipeline::auto_mask::set_subject_model(Some(Arc::new(run)));
}

#[cfg(not(feature = "sam"))]
fn install(_: &Path, _: Activity) {}
