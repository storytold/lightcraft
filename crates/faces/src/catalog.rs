//! Your own list of models to download: `catalog.json` in the models folder.
//!
//! LightCraft's built-in list only holds models whose terms let the project point at them. Anything else (a stronger
//! recogniser whose weights are for research use, a model you trained, a mirror you trust) can be added by you: each entry
//! is a model manifest (the same fields as a `face-model.json`) plus the address to fetch it from, and it then gets the same
//! Download button as a built-in one, with its licence notice shown before anything is fetched. LightCraft ships and links to
//! none of them; what the file says, and whether you may use the weights, is yours to check.
//!
//! ```json
//! { "models": [ {
//!     "id": "my-recogniser", "name": "My recogniser", "version": "1", "role": "embedder",
//!     "url": "https://example.org/weights/recogniser.onnx",
//!     "sha256": "…64 lowercase hex digits…", "sizeBytes": 166000000,
//!     "licence": { "name": "Research use only", "commercial": "no", "notice": "Not for commercial use." },
//!     "provenance": "Trained on …",
//!     "output": { "kind": "embedding", "dim": 512 },
//!     "thresholds": { "matchCosine": 0.4 }
//! } ] }
//! ```
//!
//! `input` may be left out (112 × 112 RGB, `(x − 127.5) / 127.5`, the ArcFace convention). The file is untrusted: it is read
//! with a size limit, every entry is validated, and a bad entry is reported and skipped, never a reason to refuse the rest.
//! The `sha256` and `sizeBytes` are required, since they are what a downloaded file is checked against.

use serde::Deserialize;
use serde_json::Value;

use crate::known::{self, Download};
use crate::manifest::{self, ModelManifest, Role};

/// Largest `catalog.json` we read.
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
/// Most entries we take from it.
pub const MAX_ENTRIES: usize = 50;

/// One model from the catalog: its manifest, where to fetch it and what to call the file meanwhile.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub manifest: ModelManifest,
    pub url: String,
    pub file_name: String,
}

impl Entry {
    /// What the download machinery needs. (`sha256` and `size_bytes` are present: [`parse`] requires them.)
    pub fn download(&self) -> Option<Download> {
        Some(Download {
            id: self.manifest.id.clone(),
            url: self.url.clone(),
            file_name: self.file_name.clone(),
            size_bytes: self.manifest.size_bytes?,
            sha256: self.manifest.sha256.clone()?,
        })
    }
}

/// What a catalog file holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub entries: Vec<Entry>,
    /// One plain sentence for each entry (or the file) that could not be used.
    pub errors: Vec<String>,
}

#[derive(Deserialize)]
struct Raw {
    url: String,
    #[serde(flatten)]
    manifest: ModelManifest,
}

/// A file name for the download, from the address: letters, digits, `.`, `_`, `-`, ending in `.onnx`.
fn file_name_of(url: &str) -> String {
    let last = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("");
    let clean: String = last.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(80).collect();
    if clean.ends_with(".onnx") && clean.len() > 5 && !clean.starts_with('.') { clean } else { "model.onnx".to_string() }
}

fn entry(v: &Value) -> Result<Entry, String> {
    let raw: Raw = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    manifest::validate(&raw.manifest).map_err(|e| e.to_string())?;
    let m = &raw.manifest;
    if m.role != Role::Embedder {
        return Err("only face recognition models (role \"embedder\") can be downloaded".into());
    }
    if m.sha256.is_none() || m.size_bytes.is_none() {
        return Err("needs `sha256` and `sizeBytes`: a download is checked against them".into());
    }
    if !(raw.url.len() <= 500 && raw.url.starts_with("https://") && !raw.url.chars().any(|c| c.is_control() || c.is_whitespace())) {
        return Err("`url` must be an https:// address of at most 500 characters, without spaces".into());
    }
    if known::all().iter().any(|k| k.id == m.id) {
        return Err("that id belongs to a model LightCraft already knows".into());
    }
    if m.sha256.as_deref().is_some_and(|h| known::lookup(h).is_some()) {
        return Err("LightCraft already knows a model with that file".into());
    }
    Ok(Entry { file_name: file_name_of(&raw.url), url: raw.url, manifest: raw.manifest })
}

/// Read a catalog file's bytes: the usable entries, and why the others were left out.
pub fn parse(bytes: &[u8]) -> Catalog {
    let mut out = Catalog::default();
    if bytes.len() > MAX_CATALOG_BYTES {
        out.errors.push(format!("catalog.json is larger than {} KB: not read", MAX_CATALOG_BYTES / 1024));
        return out;
    }
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => {
            out.errors.push(format!("catalog.json is not valid JSON: {e}"));
            return out;
        }
    };
    let Some(models) = value.get("models").and_then(Value::as_array) else {
        out.errors.push("catalog.json needs a \"models\" list".into());
        return out;
    };
    if models.len() > MAX_ENTRIES {
        out.errors.push(format!("only the first {MAX_ENTRIES} models of the catalog are read"));
    }
    for (i, v) in models.iter().take(MAX_ENTRIES).enumerate() {
        let label = v
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("model {}", i + 1), |id| format!("`{}`", id.chars().take(40).collect::<String>()));
        match entry(v) {
            Ok(e) if out.entries.iter().any(|x| x.manifest.id == e.manifest.id) => {
                out.errors.push(format!("{label}: listed twice, the first is used"))
            }
            Ok(e) => out.entries.push(e),
            Err(why) => out.errors.push(format!("{label}: {why}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good(id: &str) -> Value {
        json!({
            "id": id, "name": "Test model", "version": "1", "role": "embedder",
            "url": "https://example.org/w/recogniser-r50.onnx?download=1",
            "sha256": "ab".repeat(32), "sizeBytes": 1234,
            "licence": {"name": "Research only", "commercial": "no", "notice": "Not for commercial use."},
            "output": {"kind": "embedding", "dim": 512}, "thresholds": {"matchCosine": 0.4},
        })
    }

    fn cat(models: Vec<Value>) -> Catalog {
        parse(json!({"models": models}).to_string().as_bytes())
    }

    #[test]
    fn a_manifest_with_an_address_is_a_downloadable_model() {
        let c = cat(vec![good("my-model")]);
        assert_eq!(c.errors, Vec::<String>::new());
        let e = &c.entries[0];
        assert_eq!((e.manifest.id.as_str(), e.file_name.as_str()), ("my-model", "recogniser-r50.onnx"), "the file name comes from the address");
        assert_eq!(e.manifest.licence.commercial, crate::Commercial::No, "a non-commercial model is the user's to add");
        let d = e.download().unwrap();
        assert_eq!((d.size_bytes, d.sha256.len(), d.url.as_str()), (1234, 64, "https://example.org/w/recogniser-r50.onnx?download=1"));
        assert_eq!(e.manifest.input, crate::InputSpec::default(), "the input defaults to the ArcFace convention");
    }

    #[test]
    fn bad_entries_are_reported_and_the_rest_still_load() {
        let mut no_hash = good("no-hash");
        no_hash.as_object_mut().unwrap().remove("sha256");
        let mut http = good("plain-http");
        http["url"] = json!("http://example.org/m.onnx");
        let mut spaces = good("spaces");
        spaces["url"] = json!("https://example.org/a b.onnx");
        let mut detector = good("a-detector");
        detector["role"] = json!("detector");
        detector["output"] = json!({"kind": "detector", "decoder": "yunet-v2"});
        let mut bad_id = good("Bad/Id");
        bad_id["id"] = json!("../escape");
        let builtin = good("sface-2021dec");
        let mut known_file = good("known-file");
        known_file["sha256"] = json!(known::SFACE_SHA256);
        let c = cat(vec![good("fine"), no_hash, http, spaces, detector, bad_id, builtin, known_file, good("fine"), json!(7), json!({"id": "x"})]);
        assert_eq!(c.entries.iter().map(|e| e.manifest.id.as_str()).collect::<Vec<_>>(), ["fine"]);
        assert_eq!(c.errors.len(), 10, "{:#?}", c.errors);
        assert!(c.errors.iter().any(|e| e.contains("`no-hash`") && e.contains("sha256")));
        assert!(c.errors.iter().any(|e| e.contains("https")));
        assert!(c.errors.iter().any(|e| e.contains("already knows")));
        assert!(c.errors.iter().any(|e| e.contains("listed twice")));
    }

    #[test]
    fn a_hostile_or_broken_file_is_an_error_message_not_a_crash() {
        for bytes in [&b""[..], b"not json", b"[]", b"{}", b"{\"models\": 5}", b"\xff\xfe\x00", &vec![b' '; MAX_CATALOG_BYTES + 1]] {
            let c = parse(bytes);
            assert!(c.entries.is_empty() && !c.errors.is_empty(), "{:?}", String::from_utf8_lossy(&bytes[..bytes.len().min(20)]));
        }
        // more entries than allowed: the extra are ignored, with a note
        let many: Vec<Value> = (0..MAX_ENTRIES + 20).map(|i| good(&format!("m{i}"))).collect();
        let c = cat(many);
        assert_eq!(c.entries.len(), MAX_ENTRIES);
        assert!(c.errors.iter().any(|e| e.contains("first")));
        // file names from odd addresses are safe
        for url in ["https://x.org/a/b/c.onnx", "https://x.org/", "https://x.org/..%2f..%2fevil.onnx", "https://x.org/q.bin", "https://x.org/.onnx"] {
            let got = file_name_of(url);
            assert!(!got.contains(['/', '\\']) && got.ends_with(".onnx") && !got.starts_with('.'), "{url} -> {got}");
        }
    }
}
