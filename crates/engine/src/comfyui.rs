//! Opt-in masked generation using a separately operated ComfyUI server.
//!
//! A generation snapshots a developed image and its evaluated mask, works off the UI thread,
//! and keeps its result in memory until explicitly accepted. Originals and edits never change.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lightcraft_catalog::PhotoId;
use lightcraft_raster::Rgba8;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
mod client;
#[cfg(not(target_arch = "wasm32"))]
mod image;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub endpoint: String,
    pub model: String,
    pub prompt: String,
    pub negative: String,
    pub seed: u64,
    /// The instruction sent to Qwen (`.gguf` models): `{prompt}` is replaced by [`Self::prompt`].
    /// SDXL checkpoints receive the prompt alone.
    pub qwen_instruction: String,
}

/// The default [`Settings::qwen_instruction`]: names the mask reference and asks Qwen to replace
/// everything in the white region, keeping the black context unchanged.
pub const DEFAULT_QWEN_INSTRUCTION: &str = "Replace the entire white region shown in <image2> of <image1> with the following: {prompt}. \
Remove the existing objects in that white region and reconstruct it as the requested replacement. \
Black regions are context: keep them unchanged. Return an opaque photograph.";

/// The placeholder in [`Settings::qwen_instruction`] that the prompt replaces.
pub const PROMPT_PLACEHOLDER: &str = "{prompt}";

impl Default for Settings {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:8188".into(),
            model: "Juggernaut-XL_v9_RunDiffusionPhoto_v2.safetensors".into(),
            prompt: String::new(),
            negative: "blurry, low quality, text, watermark".into(),
            seed: 1,
            qwen_instruction: DEFAULT_QWEN_INSTRUCTION.into(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.prompt.trim().is_empty() || self.prompt.len() > 4096 || self.negative.len() > 4096 {
            return Err("enter a replacement prompt (at most 4096 bytes)".into());
        }
        if self.model.trim().is_empty() || self.model.len() > 512 || self.model.chars().any(char::is_control) || self.seed > 9_007_199_254_740_991 {
            return Err("choose an installed ComfyUI checkpoint and a seed from 0 to 9007199254740991".into());
        }
        if !self.qwen_instruction.contains(PROMPT_PLACEHOLDER) || self.qwen_instruction.len() > 8192 {
            return Err("the Qwen instruction must contain {prompt} (at most 8192 bytes); see Settings ▸ ComfyUI".into());
        }
        Ok(())
    }

    /// Whether the model runs the Qwen Image 2.1 workflow (a `.gguf` filename) instead of SDXL.
    pub fn is_qwen(&self) -> bool {
        self.model.to_ascii_lowercase().ends_with(".gguf")
    }

    /// The positive prompt exactly as it is sent to ComfyUI.
    pub fn resolved_prompt(&self) -> String {
        if self.is_qwen() { self.qwen_instruction.replace(PROMPT_PLACEHOLDER, &self.prompt) } else { self.prompt.clone() }
    }
}

struct Output {
    png: Vec<u8>,
    preview: Arc<Rgba8>,
    provenance: Value,
}

/// One in-flight or completed generation. No session references cross into the worker.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct Generation {
    pub id: u64,
    pub photo: PhotoId,
    pub mask_id: u32,
    pub settings: Settings,
    pub(crate) library_generation: u64,
    pub(crate) directory: std::path::PathBuf,
    pub(crate) stem: String,
    cancelled: Arc<AtomicBool>,
    finished: AtomicBool,
    result: Mutex<Option<Result<Output, String>>>,
}

impl Generation {
    pub fn belongs_to(&self, session: &crate::Session) -> bool {
        self.library_generation == session.library_generation() && session.catalog.photo(self.photo).is_some()
    }

    pub fn running(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn preview(&self) -> Option<Arc<Rgba8>> {
        if self.cancelled.load(Ordering::Acquire) {
            return None;
        }
        self.result.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(|r| r.as_ref().ok()).map(|r| r.preview.clone())
    }

    pub fn status(&self) -> Value {
        let result = self.result.lock().unwrap_or_else(PoisonError::into_inner);
        json!({"id": self.id, "photo": self.photo.0, "running": self.running(),
            "cancelled": self.cancelled.load(Ordering::Acquire),
            "ready": result.as_ref().is_some_and(Result::is_ok) && !self.cancelled.load(Ordering::Acquire),
            "error": result.as_ref().and_then(|r| r.as_ref().err())})
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn files(&self) -> Result<(Vec<u8>, Vec<u8>), String> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err("generation was cancelled".into());
        }
        let result = self.result.lock().unwrap_or_else(PoisonError::into_inner);
        let out = result.as_ref().ok_or("generation is still running")?.as_ref().map_err(Clone::clone)?;
        let provenance = serde_json::to_vec_pretty(&out.provenance).map_err(|e| e.to_string())?;
        Ok((out.png.clone(), provenance))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn start(s: &mut crate::Session, settings: Settings, mask_id: Option<u32>) -> Result<Value, String> {
    use lightcraft_catalog::Source;
    use lightcraft_pipeline::{MaskView, OutputDepth, OutputSpace, Overlay, StageCache};

    if s.generation.as_ref().is_some_and(|g| g.running()) {
        return Err("a generation is already running".into());
    }
    settings.validate()?;
    // Validate the address now; do not connect until the person explicitly generates.
    let client = client::Client::new(&settings.endpoint)?;
    let photo = s.active().ok_or("select a photo first")?;
    let mask_id = mask_id.or(s.active_mask).ok_or("select a mask first")?;
    let mask = u16::try_from(mask_id).map_err(|_| "mask id exceeds the renderer's supported range")?;
    let ph = s.catalog.photo(photo).ok_or("photo is no longer in the library")?;
    if !ph.develop.masks.iter().any(|m| m.id == mask_id && !m.components.is_empty()) {
        return Err("the selected mask has no components".into());
    }
    if ph.develop.hdr.enabled {
        return Err("ComfyUI generation currently requires an SDR photo".into());
    }
    let (w, h) = crate::export::output_size(ph, &Default::default());
    if w.checked_mul(h).is_none_or(|n| n > 80_000_000) {
        return Err("generation currently supports photos up to 80 megapixels".into());
    }
    let (directory, stem) = match &ph.source {
        Source::File { path } => {
            let p = std::path::Path::new(path);
            (p.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf(), p.file_stem().unwrap_or_default().to_string_lossy().into_owned())
        }
        Source::Demo { .. } => {
            (s.library.as_ref().map(|l| l.dir.join("Generated")).unwrap_or_else(|| std::env::temp_dir().join("LightCraft-generated")), "Demo".into())
        }
    };
    let original = json!({"photo": photo.0, "source": ph.source, "develop": ph.develop});
    let job = s.export_job(photo, w, h, OutputSpace::Srgb, OutputDepth::U8)?.with_stages(Arc::new(StageCache::default()));
    let overlay = Overlay::Mask { id: mask, view: MaskView::WhiteOnBlack, color: [255; 3], opacity: 100 };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let run = Arc::new(Generation {
        id: NEXT.fetch_add(1, Ordering::Relaxed),
        photo,
        mask_id,
        settings,
        library_generation: s.library_generation(),
        directory,
        stem,
        cancelled: Arc::new(AtomicBool::new(false)),
        finished: AtomicBool::new(false),
        result: Mutex::new(None),
    });
    let guard = s.activity.start("generation", "Generating replacement", crate::activity::Cancel::Flag(run.cancelled.clone()));
    guard.detail(&run.stem);
    let worker = run.clone();
    std::thread::Builder::new()
        .name("comfyui-generation".into())
        .spawn(move || {
            let result = crate::guard::catch("ComfyUI generation", || {
                let cancelled = || worker.cancelled.load(Ordering::Acquire);
                let check = || if cancelled() { Err("generation cancelled".to_string()) } else { Ok(()) };
                check()?;
                let mut mask_job = job.clone().with_overlay(overlay);
                let rendered = job.run();
                if let Some(loaded) = rendered.loaded {
                    mask_job.source = crate::media::SourceRef::Loaded(Box::new(loaded));
                }
                let original_image = rendered.rendered?.image;
                check()?;
                let mask = mask_job.run().rendered?.image;
                let region = image::region(&mask)?;
                let input = image::encode(&image::crop(&original_image, region)?)?;
                let mask_input = image::encode(&image::crop(&mask, region)?)?;
                check()?;
                let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
                let prefix = format!("Lightcraft-{}-{nonce}-{}", std::process::id(), worker.id);
                let input_name = client.upload(&format!("{prefix}-image.png"), &input)?;
                check()?;
                let mask_name = client.upload(&format!("{prefix}-mask.png"), &mask_input)?;
                let bytes = client.generate(&worker.settings, &input_name, &mask_name, cancelled)?;
                check()?;
                let decoded = lightcraft_codecs::decode(&bytes, lightcraft_codecs::DecodeOptions { max_pixels: 1024 * 1024, ..Default::default() })
                    .map_err(|e| e.to_string())?;
                if decoded.format != lightcraft_codecs::Format::Png || decoded.orientation != 1 {
                    return Err("ComfyUI must return an unoriented PNG".into());
                }
                let output = image::composite(original_image, &mask, &decoded.to_srgb8(), region)?;
                let preview = Arc::new(crate::media::fit_for_display(&output, 1024, 768));
                let provenance = json!({"workflow": "Lightcraft-masking", "version": 1, "settings": worker.settings, "sentPrompt": worker.settings.resolved_prompt(),
                "maskId": mask_id, "original": original, "region": region,
                "width": output.width, "height": output.height,
                "created": chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now()).to_rfc3339()});
                let png = image::encode(&output)?;
                check()?;
                Ok(Output { png, preview, provenance })
            })
            .and_then(|r| r);
            *worker.result.lock().unwrap_or_else(PoisonError::into_inner) = Some(result);
            drop(guard);
            worker.finished.store(true, Ordering::Release);
        })
        .map_err(|e| format!("could not start ComfyUI worker: {e}"))?;
    let status = run.status();
    s.generation = Some(run);
    Ok(status)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn start(_: &mut crate::Session, _: Settings, _: Option<u32>) -> Result<Value, String> {
    Err("ComfyUI generation is available in the desktop app only".into())
}

#[cfg(test)]
mod tests;
