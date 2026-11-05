use super::gpu::GPU_SETUP_MARKER;
use super::model::RunOn;
use super::{DenoiserManifest, Deserialize, Duration, Path, PathBuf, Serialize, Value};

/// Extension of a product file.
pub const EXT: &str = "lcdn";

/// Where the products of a library are kept, inside the library's folder (safe to delete, like `thumbs/`).
pub const DIR: &str = "denoise";

/// What the cache may take up unless the user says otherwise (GB). A product is about as large as the raw file's
/// decoded samples, compressed: ~100 MB for 24 megapixels.
pub const DEFAULT_CACHE_GB: u32 = 20;

/// How long a look at the model settings is trusted.
pub(super) const SETTINGS_TTL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------------------------------------------------
// settings and installed models

/// `settings.json` in the models folder.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct Settings {
    /// The installed model in use; `None`: denoise is off.
    pub model: Option<String>,
    /// Photos with a Denoise amount above 0 that are being looked at (active, selected) get their picture made without
    /// being asked (`None`: yes).
    pub auto: Option<bool>,
    /// Cache size limit (GB; `None`: [`DEFAULT_CACHE_GB`]).
    pub cache_gb: Option<u32>,
    /// Most tiles run at once (`None`: by how hard the pump says to work).
    pub threads: Option<u32>,
    /// Where the model runs: `auto`, `gpu` or `cpu` (`None` or anything else: `auto`; see [`RunOn`]).
    pub run_on: Option<String>,
    /// What came before `runOn`: `false` was the processor only.
    pub gpu: Option<bool>,
}

impl Settings {
    pub fn auto(&self) -> bool {
        self.auto.unwrap_or(true)
    }

    pub fn run_on(&self) -> RunOn {
        match self.run_on.as_deref().and_then(RunOn::parse) {
            Some(r) => r,
            None if self.gpu == Some(false) => RunOn::Cpu,
            None => RunOn::Auto,
        }
    }

    pub fn cache_bytes(&self) -> u64 {
        u64::from(self.cache_gb.unwrap_or(DEFAULT_CACHE_GB).clamp(1, 10_000)) << 30
    }
}

pub(crate) fn read_capped(path: &Path, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(max as u64 + 1).read_to_end(&mut buf).ok()?;
    (buf.len() <= max).then_some(buf)
}

pub(crate) fn read_settings(dir: &Path) -> Settings {
    read_capped(&dir.join("settings.json"), 4096).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Write a file next to its final name and rename it into place.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let part = path.with_extension("part");
    std::fs::write(&part, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    std::fs::rename(&part, path).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        format!("could not write {}: {e}", path.display())
    })
}

pub(crate) fn write_settings(dir: &Path, st: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create the denoise models folder: {e}"))?;
    write_atomic(&dir.join("settings.json"), &serde_json::to_vec_pretty(st).map_err(|e| e.to_string())?)
}

/// An installed model folder: `<id>/model.onnx`, `denoise-model.json`, `installed.json`.
pub(crate) struct Installed {
    pub manifest: DenoiserManifest,
    pub onnx: PathBuf,
    /// What was recorded when it was installed (terms accepted, hash of the file, self-test).
    pub accepted: Value,
}

impl Installed {
    /// Names the model file the cache is made with: a hash of the file as installed, so another file with the same id
    /// never finds this one's products.
    pub fn fingerprint(&self) -> String {
        let sha = self.accepted.get("sha256").and_then(Value::as_str).or(self.manifest.sha256.as_deref()).unwrap_or("");
        let len = std::fs::metadata(&self.onnx).map(|m| m.len()).unwrap_or(0);
        format!("{}@{}:{len}", self.manifest.id, sha)
    }
}

/// The models in the folder. A folder that is not a valid model (wrong name, no model file, a manifest that does not
/// validate or does not match its folder) is ignored, never trusted.
pub(crate) fn installed_models(dir: &Path) -> Vec<Installed> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(str::to_string) else { continue };
        let onnx = p.join("model.onnx");
        if !lightcraft_denoise::licence::valid_id(&name) || !p.is_dir() || !onnx.is_file() {
            continue;
        }
        let Some(m) = read_capped(&p.join("denoise-model.json"), lightcraft_denoise::manifest::MAX_MANIFEST_BYTES)
            .and_then(|b| lightcraft_denoise::manifest::parse(&b).ok())
        else {
            continue;
        };
        if m.id != name {
            continue;
        }
        let accepted = read_capped(&p.join("installed.json"), 8192).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        out.push(Installed { manifest: m, onnx, accepted });
    }
    out.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    out
}

/// Forget that setting the graphics card up for a model closed LightCraft before (the markers [`set_up_card`] leaves):
/// the user chose where denoise runs, so the card may be tried again.
pub(crate) fn forget_gpu_set_ups(dir: &Path) {
    for i in installed_models(dir) {
        let _ = std::fs::remove_file(i.onnx.with_file_name(GPU_SETUP_MARKER));
    }
}
