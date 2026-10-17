//! Soft proofing against an output-device ICC profile (a printer and paper): loads the profile,
//! builds its proof table through the CMS and registers it with the pipeline, which a
//! [`Proof`](lcc_pipeline::Proof) then names by id.

use lcc_codecs::display::MAX_PROFILE_BYTES;
pub use lcc_codecs::proof::ProofIntent;

/// A device profile ready to proof against.
#[derive(Clone, Debug, PartialEq)]
pub struct ProofProfile {
    /// Pass as [`Proof::profile`](lcc_pipeline::Proof::profile).
    pub id: u64,
    /// The profile's description, else the file name.
    pub name: String,
}

/// Load the profile at `path` for proofing with `intent`, simulating paper and ink or not.
pub fn load(path: &str, intent: ProofIntent, paper_ink: bool) -> Result<ProofProfile, String> {
    let path = path.trim();
    let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
    if meta.len() > MAX_PROFILE_BYTES as u64 {
        return Err(format!("{path}: too large for an ICC profile"));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let fallback = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    from_bytes(&bytes, intent, paper_ink, &fallback).map_err(|e| format!("{path}: {e}"))
}

/// [`load`] from the profile's bytes; `fallback_name` names a profile without a description.
pub fn from_bytes(bytes: &[u8], intent: ProofIntent, paper_ink: bool, fallback_name: &str) -> Result<ProofProfile, String> {
    let table = lcc_codecs::proof::proof_table(bytes, intent, paper_ink)?;
    let lut = lcc_pipeline::ProofLut::new(table.n, table.data).ok_or("the profile's tables give no usable colours")?;
    let id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (bytes, intent, paper_ink).hash(&mut h);
        h.finish()
    };
    let id = lcc_pipeline::register_proof_profile(id, lut);
    Ok(ProofProfile { id, name: table.description.unwrap_or_else(|| fallback_name.to_string()) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_codecs::space::NamedSpace;

    #[test]
    fn profiles_register_by_content_and_options() {
        let p3 = lcc_codecs::icc::write_named(NamedSpace::DisplayP3);
        let a = from_bytes(&p3, ProofIntent::Perceptual, false, "x").unwrap();
        let b = from_bytes(&p3, ProofIntent::Perceptual, true, "x").unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a, from_bytes(&p3, ProofIntent::Perceptual, false, "x").unwrap());
        assert!(lcc_pipeline::proof_profile(a.id).is_some());
        assert!(!a.name.is_empty());
        assert!(from_bytes(b"nope", ProofIntent::Relative, false, "x").is_err());
        assert!(load("/nonexistent/printer.icc", ProofIntent::Relative, false).is_err());
        assert_eq!(ProofIntent::parse("Relative"), Some(ProofIntent::Relative));
        assert_eq!(ProofIntent::parse("saturation"), None);
    }
}
