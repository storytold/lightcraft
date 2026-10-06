//! What to do with a model file the user drops in: recognise it by hash, or look at its shape and propose a
//! manifest the user confirms, or say plainly why it cannot be used yet.

use crate::manifest::{Commercial, InputSpec, Licence, ModelManifest, OutputSpec, Role, Thresholds};
use crate::onnx::{Dim, OnnxInfo, TensorInfo};

/// ONNX element type for float32.
const FLOAT32: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub enum Suggestion {
    /// A model we know by its SHA-256: its manifest as it is.
    Known(ModelManifest),
    /// An unknown model shaped like a face embedder. `assumptions` is what we guessed, in plain words.
    Draft { manifest: ModelManifest, assumptions: Vec<String> },
    /// Not usable yet, and why.
    Unsupported(String),
}

fn fixed(d: &Dim) -> Option<u64> {
    match d {
        Dim::Fixed(v) => Some(*v),
        Dim::Dynamic(_) => None,
    }
}

/// An id made of the file name and the start of the hash: lowercase letters, digits, `.` `_` `-`.
fn make_id(file_name: &str, sha256: &str) -> String {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    let clean: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let clean: String = clean.trim_matches('-').chars().take(40).collect();
    let short: String = sha256.chars().take(8).collect();
    match (clean.is_empty(), short.is_empty()) {
        (true, true) => "custom".into(),
        (true, false) => format!("custom-{short}"),
        (false, true) => format!("custom-{clean}"),
        (false, false) => format!("custom-{clean}-{short}"),
    }
}

fn display_name(file_name: &str) -> String {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    let name: String = stem.chars().filter(|c| !c.is_control()).take(60).collect();
    if name.trim().is_empty() { "Custom face model".into() } else { format!("{} (your model)", name.trim()) }
}

/// The input of a face embedder: float32, `[batch, 3, height, width]`.
fn face_input(t: &TensorInfo) -> Option<(u32, u32)> {
    if t.elem_type != FLOAT32 || t.shape.len() != 4 || t.shape.get(1).and_then(fixed) != Some(3) {
        return None;
    }
    let size = |d: Option<&Dim>| match d.and_then(fixed) {
        Some(v) if (16..=1024).contains(&v) => u32::try_from(v).ok(),
        Some(_) => None,
        None => Some(112), // a dynamic size: the common face-crop size
    };
    Some((size(t.shape.get(3))?, size(t.shape.get(2))?))
}

/// Propose what to do with a probed model. `sha256` and `size` describe the file, `file_name` names it.
pub fn suggest(info: &OnnxInfo, sha256: &str, size: u64, file_name: &str) -> Suggestion {
    suggest_with(info, sha256, size, file_name, &[])
}

/// [`suggest`], also recognising the models in `extra` (the user's own catalog) by their SHA-256.
pub fn suggest_with(info: &OnnxInfo, sha256: &str, size: u64, file_name: &str, extra: &[ModelManifest]) -> Suggestion {
    if let Some(m) = crate::known::lookup(sha256).or_else(|| extra.iter().find(|m| m.sha256.as_deref() == Some(sha256)).cloned()) {
        return Suggestion::Known(m);
    }
    let [input] = info.inputs.as_slice() else {
        return Suggestion::Unsupported(format!("the model has {} inputs; face models here take one image input", info.inputs.len()));
    };
    let Some((width, height)) = face_input(input) else {
        return Suggestion::Unsupported("its input is not a float image of shape [batch, 3, height, width]".into());
    };
    let [output] = info.outputs.as_slice() else {
        return Suggestion::Unsupported(
            "it has several outputs, like a face detector; only models that give one vector per face are supported for now".into(),
        );
    };
    if output.elem_type != FLOAT32 || output.shape.len() != 2 {
        return Suggestion::Unsupported("its output is not one vector per face (shape [batch, dimension])".into());
    }
    let dim = match output.shape.get(1).and_then(fixed).map(u32::try_from) {
        Some(Ok(d)) if (8..=4096).contains(&d) => d,
        _ => return Suggestion::Unsupported("its output size is not a fixed number between 8 and 4096".into()),
    };
    let manifest = ModelManifest {
        id: make_id(file_name, sha256),
        name: display_name(file_name),
        version: "1".into(),
        role: Role::Embedder,
        licence: Licence {
            name: "Unknown".into(),
            commercial: Commercial::Unknown,
            url: None,
            notice: "LightCraft does not know where this model came from or what its licence allows. Use it only if its licence allows what you are doing, and do not redistribute it.".into(),
        },
        source: None,
        sha256: Some(sha256.to_string()).filter(|h| h.len() == 64),
        size_bytes: Some(size).filter(|s| *s > 0),
        provenance: "Unknown.".into(),
        input: InputSpec { width, height, ..InputSpec::default() },
        output: OutputSpec::Embedding { dim },
        thresholds: Thresholds::default(),
        speed: None,
    };
    let assumptions = vec![
        format!("Faces are aligned crops of {width} × {height} pixels."),
        "Pixels are RGB, normalised as (x − 127.5) / 127.5: the usual ArcFace convention, which InsightFace models use.".into(),
        format!("Each face becomes a vector of {dim} numbers; two faces match when their cosine similarity is high."),
    ];
    Suggestion::Draft { manifest, assumptions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::validate;
    use crate::onnx::probe_reader;
    use std::io::Cursor;

    fn tensor(elem: u32, shape: &[Dim]) -> TensorInfo {
        TensorInfo { name: "t".into(), elem_type: elem, shape: shape.to_vec() }
    }
    fn dynamic() -> Dim {
        Dim::Dynamic("N".into())
    }
    fn info(input: TensorInfo, output: TensorInfo) -> OnnxInfo {
        OnnxInfo { producer: "x".into(), opset: Some(17), inputs: vec![input], outputs: vec![output] }
    }
    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_known_hash_wins() {
        let i = info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(10), Dim::Fixed(10)]), tensor(1, &[dynamic(), Dim::Fixed(5)]));
        assert!(matches!(suggest(&i, crate::known::AURAFACE_SHA256, 1, "whatever.onnx"), Suggestion::Known(m) if m.id == "auraface-v1"));
    }

    #[test]
    fn an_unknown_embedder_gets_a_draft_that_validates() {
        let i = info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(512)]));
        let Suggestion::Draft { manifest, assumptions } = suggest(&i, SHA, 5000, "My Model (r50).onnx") else { unreachable_in_test() };
        assert_eq!(validate(&manifest), Ok(()));
        assert_eq!(manifest.id, "custom-my-model--r50-01234567");
        assert_eq!(manifest.output, OutputSpec::Embedding { dim: 512 });
        assert_eq!((manifest.input.width, manifest.input.height), (112, 112));
        assert_eq!(manifest.licence.commercial, Commercial::Unknown);
        assert!(assumptions.iter().any(|a| a.contains("127.5")));
    }

    #[allow(clippy::panic)]
    fn unreachable_in_test() -> ! {
        panic!("expected a draft");
    }

    #[test]
    fn dynamic_sizes_default_and_odd_shapes_are_refused() {
        let ok = info(tensor(1, &[dynamic(), Dim::Fixed(3), dynamic(), dynamic()]), tensor(1, &[dynamic(), Dim::Fixed(128)]));
        assert!(matches!(suggest(&ok, SHA, 1, "a.onnx"), Suggestion::Draft { manifest, .. } if manifest.input.width == 112));
        let refused = [
            info(tensor(1, &[dynamic(), Dim::Fixed(1), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(128)])), // grey
            info(tensor(7, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(128)])), // int64 input
            info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(128)])),                  // rank 3
            info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(1_000_000), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(128)])),
            info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(257), Dim::Fixed(384)])), // tokens
            info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), dynamic()])),
            info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(2)])),
            info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(1_000_000)])),
        ];
        for r in refused {
            assert!(matches!(suggest(&r, SHA, 1, "a.onnx"), Suggestion::Unsupported(_)), "{r:?}");
        }
        let mut two = ok.clone();
        two.outputs.push(two.outputs[0].clone());
        assert!(matches!(suggest(&two, SHA, 1, "a.onnx"), Suggestion::Unsupported(_)), "several outputs");
    }

    #[test]
    fn file_names_cannot_break_the_manifest() {
        let i = info(tensor(1, &[dynamic(), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]), tensor(1, &[dynamic(), Dim::Fixed(512)]));
        for name in ["", ".onnx", "../../etc/passwd", "\u{7}\u{0}x.onnx", &"x".repeat(5000), "ÅÄÖ 🙂.onnx", "UPPER lower.ONNX"] {
            let Suggestion::Draft { manifest, .. } = suggest(&i, SHA, 1, name) else { unreachable_in_test() };
            assert_eq!(validate(&manifest), Ok(()), "{name:?}");
        }
        // and with no usable hash at all
        let Suggestion::Draft { manifest, .. } = suggest(&i, "", 0, "x.onnx") else { unreachable_in_test() };
        assert_eq!(validate(&manifest), Ok(()));
        assert!(manifest.sha256.is_none() && manifest.size_bytes.is_none());
    }

    /// Opt-in: `LC_FACE_MODELS=/folder/with/onnx/files cargo test -p lightcraft-faces -- --ignored --nocapture`
    /// probes real models (never committed) and prints what would be suggested, with timings.
    #[test]
    #[ignore = "needs real model files: set LC_FACE_MODELS"]
    fn probing_real_models() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS") else { return };
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "onnx") {
                continue;
            }
            let t = std::time::Instant::now();
            let sha = crate::hash::sha256_file(&path).unwrap();
            let hashed = t.elapsed();
            let t = std::time::Instant::now();
            let info = crate::onnx::probe_path(&path);
            let probed = t.elapsed();
            let size = std::fs::metadata(&path).unwrap().len();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let verdict = match &info {
                Ok(i) => match suggest(i, &sha, size, &name) {
                    Suggestion::Known(m) => format!("KNOWN {} ({:?})", m.id, m.licence.commercial),
                    Suggestion::Draft { manifest, .. } => format!("DRAFT {} {:?}", manifest.id, manifest.output),
                    Suggestion::Unsupported(why) => format!("UNSUPPORTED: {why}"),
                },
                Err(e) => format!("PROBE ERROR: {e}"),
            };
            println!("{name}: {} MB | hash {hashed:?} | probe {probed:?} | {verdict}", size / 1_048_576);
            if let Ok(i) = info {
                println!(
                    "   in {:?}
   out {:?}",
                    i.inputs, i.outputs
                );
            }
        }
    }

    #[test]
    fn probing_then_suggesting_end_to_end() {
        let probed = probe_reader(&mut Cursor::new(crate::onnx::tests::model_bytes(512))).unwrap();
        assert!(
            matches!(suggest(&probed, SHA, 9, "m.onnx"), Suggestion::Draft { manifest, .. } if manifest.output == OutputSpec::Embedding { dim: 512 })
        );
    }
}
