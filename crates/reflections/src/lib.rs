//! DSRNet transmission inference, ported from mingcv/DSRNet (Apache-2.0).
//! Modified work: LightCraft contributors, 2026. See NOTICE. No PyTorch process at runtime.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
mod net;
#[cfg(not(target_arch = "wasm32"))]
pub use net::Net;

pub const MAX_EDGE: usize = 1024;
pub const WEIGHTS: &str = "model.safetensors";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub architecture: String,
    pub sha256: String,
    pub source_sha256: String,
    pub vgg_sha256: String,
}
pub fn installed(dir: &Path) -> bool {
    dir.join(WEIGHTS).is_file() && dir.join("manifest.json").is_file()
}
pub fn manifest(dir: &Path) -> Result<Manifest, String> {
    use std::io::Read;
    let file = std::fs::File::open(dir.join("manifest.json")).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 8192 {
        return Err("reflection model manifest too large".into());
    }
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if m.version != 1
        || !matches!(m.architecture.as_str(), "dsrnet-s" | "dsrnet-l" | "dsrnet-l-nature")
        || m.sha256.len() != 64
        || !m.sha256.bytes().all(|x| x.is_ascii_hexdigit())
    {
        return Err("unsupported reflection manifest".into());
    }
    Ok(m)
}
pub fn verified_weights(dir: &Path) -> Result<(Manifest, Vec<u8>), String> {
    use std::io::Read;
    let m = manifest(dir)?;
    let f = std::fs::File::open(dir.join(WEIGHTS)).map_err(|e| e.to_string())?;
    let n = f.metadata().map_err(|e| e.to_string())?.len();
    if !(16..=768 * 1024 * 1024).contains(&n) {
        return Err("invalid reflection model size".into());
    }
    let mut bytes = Vec::with_capacity(n as usize);
    f.take(n + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != n || format!("{:x}", Sha256::digest(&bytes)) != m.sha256.to_ascii_lowercase() {
        return Err("reflection model checksum mismatch".into());
    }
    Ok((m, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_manifest_and_bad_checksums_are_rejected() {
        let dir = std::env::temp_dir().join(format!("lc-reflection-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), vec![b' '; 8193]).unwrap();
        assert!(manifest(&dir).unwrap_err().contains("too large"));
        let m = Manifest {
            version: 1,
            architecture: "dsrnet-s".into(),
            sha256: "0".repeat(64),
            source_sha256: "0".repeat(64),
            vgg_sha256: "0".repeat(64),
        };
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        std::fs::write(dir.join(WEIGHTS), vec![0u8; 16]).unwrap();
        assert!(verified_weights(&dir).unwrap_err().contains("checksum mismatch"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
