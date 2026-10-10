//! Installed recognition models on the app's own checked, single-thread float32 CPU engine.
//! The detector and recognisers share graph validation and convolution kernels.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use web_time::Instant;

use crate::{
    graph,
    net::{Net, Tensor},
};
use serde::Serialize;

use crate::manifest::{Colour, ModelManifest, OutputSpec};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("the model could not be loaded: {0}")]
    Load(String),
    #[error("the model failed while running: {0}")]
    Run(String),
    #[error("{0}")]
    Input(String),
}

/// What `self_test` found.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelfTest {
    pub ok: bool,
    /// Milliseconds to load and optimise the model.
    pub load_ms: f64,
    /// Median milliseconds to embed one face.
    pub embed_ms: f64,
    pub dimension: usize,
    /// Each check: what was checked and whether it held.
    pub checks: Vec<(String, bool)>,
}

type Plan = Net;

/// A loaded recognition model.
#[derive(Clone)]
pub struct Embedder {
    plan: Arc<Plan>,
    manifest: ModelManifest,
    width: usize,
    height: usize,
    dim: usize,
    load_ms: f64,
}

fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, RuntimeError>) -> Result<T, RuntimeError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(RuntimeError::Run(format!("the runtime gave up on this model while {what}"))),
    }
}

impl Embedder {
    /// Load the `.onnx` at `path` as described by `manifest` (an embedder).
    pub fn load(path: &Path, manifest: &ModelManifest) -> Result<Embedder, RuntimeError> {
        let OutputSpec::Embedding { dim } = manifest.output else {
            return Err(RuntimeError::Load("this is not a recognition model".into()));
        };
        let (w, h) = (manifest.input.width as usize, manifest.input.height as usize);
        let started = Instant::now();
        crate::manifest::validate(manifest).map_err(|e| RuntimeError::Load(e.to_string()))?;
        let mut file = std::fs::File::open(path).map_err(|e| RuntimeError::Load(e.to_string()))?;
        let length = file.metadata().map_err(|e| RuntimeError::Load(e.to_string()))?.len();
        if length == 0 || length > 512 * 1024 * 1024 {
            return Err(RuntimeError::Load("the model file is empty or exceeds 512 MiB".into()));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(usize::try_from(length).map_err(|_| RuntimeError::Load("model size overflow".into()))?)
            .map_err(|_| RuntimeError::Load("not enough memory for the model".into()))?;
        use std::io::Read;
        (&mut file).take(512 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| RuntimeError::Load(e.to_string()))?;
        let graph = graph::load(&bytes).map_err(|e| RuntimeError::Load(e.to_string()))?;
        let input = graph.inputs.first().ok_or_else(|| RuntimeError::Load("no input".into()))?;
        let want = [1, 3, h, w];
        if input.1.len() != 4 || input.1.iter().zip(want).any(|(&d, w)| d != 0 && d != w) {
            return Err(RuntimeError::Load("the graph input differs from the manifest's [1,3,H,W]".into()));
        }
        let plan = Net::new(graph).map_err(|e| RuntimeError::Load(e.to_string()))?;
        let shapes = plan.validate_input(&want).map_err(|e| RuntimeError::Load(e.to_string()))?;
        if shapes.len() != 1 || shapes.first().is_none_or(|s| s.first() != Some(&1) || s.iter().product::<usize>() != dim as usize) {
            return Err(RuntimeError::Load("the graph output differs from the manifest's embedding dimension".into()));
        }
        Ok(Embedder {
            plan: Arc::new(plan),
            manifest: manifest.clone(),
            width: w,
            height: h,
            dim: dim as usize,
            load_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    /// The size of the aligned face picture the model wants (width, height).
    pub fn input_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn manifest(&self) -> &ModelManifest {
        &self.manifest
    }

    /// The face as a unit-length vector. `rgb` is an aligned face at exactly [`Self::input_size`], 8-bit RGB.
    pub fn embed(&self, rgb: &[u8]) -> Result<Vec<f32>, RuntimeError> {
        if rgb.len() != self.width * self.height * 3 {
            return Err(RuntimeError::Input("the face picture is not the size the model wants".into()));
        }
        let (mean, std, bgr) = (self.manifest.input.mean, self.manifest.input.std, self.manifest.input.colour == Colour::Bgr);
        let plane = self.width * self.height;
        let mut data = vec![0.0f32; 3 * plane];
        for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
            for c in 0..3 {
                let src = if bgr { 2 - c } else { c };
                if let (Some(v), Some(m), Some(s), Some(o)) = (px.get(src), mean.get(c), std.get(c), data.get_mut(c * plane + i)) {
                    *o = (f32::from(*v) - m) / s;
                }
            }
        }
        let out = guarded("running it", || {
            let input = Tensor::new(vec![1, 3, self.height, self.width], data).map_err(|e| RuntimeError::Input(e.to_string()))?;
            let mut result = self.plan.run(input).map_err(|e| RuntimeError::Run(e.to_string()))?;
            if result.len() != 1 {
                return Err(RuntimeError::Run("the model must give one embedding".into()));
            }
            Ok(result.pop().ok_or_else(|| RuntimeError::Run("no output".into()))?.1.data)
        })?;
        if out.len() != self.dim {
            return Err(RuntimeError::Run(format!("the model gave {} numbers per face, not the {} its description says", out.len(), self.dim)));
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(RuntimeError::Run("the model gave numbers that are not finite".into()));
        }
        let norm = out.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt();
        if !norm.is_finite() || norm < 1e-9 {
            return Err(RuntimeError::Run("the model gave an all-zero face".into()));
        }
        Ok(out.into_iter().map(|v| (f64::from(v) / norm) as f32).collect())
    }

    /// Check the model does something sensible before it is ever used on a photo: the right number of finite
    /// numbers, the same picture twice gives the same face, and two different pictures give different faces.
    pub fn self_test(&self) -> SelfTest {
        let mut checks: Vec<(String, bool)> = Vec::new();
        let n = self.width * self.height * 3;
        // two deterministic, different test pictures: noise, and a diagonal gradient with a colour cast
        let mut seed = 0x2545_f491u32;
        let noise: Vec<u8> = (0..n)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed >> 8) as u8
            })
            .collect();
        let gradient: Vec<u8> = (0..n)
            .map(|i| (((i / 3) % self.width + (i / 3) / self.width) * 255 / (self.width + self.height) + (i % 3) * 40).min(255) as u8)
            .collect();
        let mut times = Vec::new();
        let mut run = |img: &[u8]| {
            let t = Instant::now();
            let r = self.embed(img);
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            r
        };
        let (a, a2, b) = (run(&noise), run(&noise), run(&gradient));
        let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        match (&a, &a2, &b) {
            (Ok(a), Ok(a2), Ok(b)) => {
                checks.push((format!("gives {} finite numbers per face", a.len()), true));
                checks.push(("gives the same face for the same picture".into(), dot(a, a2) > 0.9999));
                checks.push(("gives different faces for different pictures".into(), dot(a, b) < 0.9995));
            }
            (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => checks.push((format!("runs: {e}"), false)),
        }
        times.sort_by(|x, y| x.total_cmp(y));
        SelfTest {
            ok: checks.iter().all(|(_, ok)| *ok),
            load_ms: self.load_ms,
            embed_ms: times.get(times.len() / 2).copied().unwrap_or(0.0),
            dimension: self.dim,
            checks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{InputSpec, Licence, OutputSpec, Role, Thresholds};

    fn manifest(dim: u32) -> ModelManifest {
        ModelManifest {
            id: "tiny".into(),
            name: "Tiny".into(),
            version: "1".into(),
            role: Role::Embedder,
            licence: Licence::default(),
            source: None,
            sha256: None,
            size_bytes: None,
            provenance: String::new(),
            input: InputSpec::default(),
            output: OutputSpec::Embedding { dim },
            thresholds: Thresholds::default(),
            speed: None,
        }
    }

    fn temp_model(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lc-faces-runtime-{name}-{}.onnx", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn a_working_model_loads_embeds_and_passes_the_self_test() {
        let p = temp_model("ok", &crate::synthetic::tiny_embedder_model(16));
        let e = Embedder::load(&p, &manifest(16)).unwrap();
        assert_eq!(e.input_size(), (112, 112));
        let v = e.embed(&vec![100u8; 112 * 112 * 3]).unwrap();
        assert_eq!(v.len(), 16);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4, "unit length");
        let t = e.self_test();
        assert!(t.ok, "{t:?}");
        assert_eq!(t.dimension, 16);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn wrong_sizes_wrong_dimensions_and_broken_files_are_errors() {
        let p = temp_model("dim", &crate::synthetic::tiny_embedder_model(16));
        let e = Embedder::load(&p, &manifest(16)).unwrap();
        assert!(e.embed(&[0u8; 10]).is_err(), "wrong picture size");
        // the manifest promises 32 numbers, the model gives 16
        assert!(Embedder::load(&p, &manifest(32)).is_err());
        // not a model, an empty file, a graph with no layers, a detector-shaped manifest
        for (name, bytes) in [("junk", b"nonsense".to_vec()), ("empty", Vec::new()), ("nolayers", crate::synthetic::embedder_model(16))] {
            let q = temp_model(name, &bytes);
            assert!(Embedder::load(&q, &manifest(16)).is_err(), "{name}");
            let _ = std::fs::remove_file(q);
        }
        let mut detector = manifest(16);
        detector.output = OutputSpec::Detector { decoder: "yunet-v2".into() };
        assert!(Embedder::load(&p, &detector).is_err());
        assert!(Embedder::load(&std::env::temp_dir().join("does-not-exist.onnx"), &manifest(16)).is_err());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn truncated_and_corrupted_models_never_panic() {
        let good = crate::synthetic::tiny_embedder_model(16);
        for n in (0..good.len()).step_by(7) {
            let q = temp_model("trunc", &good[..n]);
            let _ = Embedder::load(&q, &manifest(16));
            let _ = std::fs::remove_file(q);
        }
        for at in (0..good.len()).step_by(11) {
            let mut bad = good.clone();
            if let Some(b) = bad.get_mut(at) {
                *b = b.wrapping_add(0x55);
            }
            let q = temp_model("flip", &bad);
            if let Ok(e) = Embedder::load(&q, &manifest(16)) {
                let _ = e.embed(&vec![7u8; 112 * 112 * 3]);
            }
            let _ = std::fs::remove_file(q);
        }
    }

    /// Opt-in: `LC_FACE_MODELS=<folder> cargo test -p dac-faces -- --ignored --nocapture`
    /// self-tests every known recognition model found there.
    #[test]
    #[ignore = "needs real model files: set LC_FACE_MODELS"]
    fn real_models_pass_the_self_test() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS") else { return };
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let Ok(sha) = crate::hash::sha256_file(&path) else { continue };
            let Some(m) = crate::known::lookup(&sha).filter(|m| m.role == Role::Embedder) else { continue };
            let e = Embedder::load(&path, &m).unwrap();
            let t = e.self_test();
            println!("{}: ok {} | load {:.0} ms | {:.0} ms per face | {} numbers | {:?}", m.id, t.ok, t.load_ms, t.embed_ms, t.dimension, t.checks);
            assert!(t.ok);
        }
    }

    #[test]
    fn bgr_changes_the_input() {
        let p = temp_model("bgr", &crate::synthetic::tiny_embedder_model(16));
        let rgb = Embedder::load(&p, &manifest(16)).unwrap();
        let mut m = manifest(16);
        m.input.colour = crate::manifest::Colour::Bgr;
        let bgr = Embedder::load(&p, &m).unwrap();
        // a picture with different channel levels: swapping the order must change the embedding
        let img: Vec<u8> = (0..112 * 112).flat_map(|_| [200u8, 100, 20]).collect();
        let (a, b) = (rgb.embed(&img).unwrap(), bgr.embed(&img).unwrap());
        let cos: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!(cos < 0.9999, "{cos}");
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn large_finite_embeddings_have_a_finite_unit_norm() {
        let p = temp_model("large-norm", &crate::synthetic::norm_overflow_model());
        let e = Embedder::load(&p, &manifest(3)).unwrap();
        let out = e.embed(&vec![255; 112 * 112 * 3]).unwrap();
        assert!(out.iter().all(|v| v.is_finite()));
        assert!((out.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-6);
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn invalid_manifest_numbers_are_rejected_before_preprocessing() {
        let p = temp_model("invalid-manifest", &crate::synthetic::tiny_embedder_model(16));
        for case in 0..5 {
            let mut m = manifest(16);
            match case {
                0 => m.input.std = [0.0; 3],
                1 => m.input.mean = [f32::NAN; 3],
                2 => m.input.width = 0,
                3 => m.input.width = u32::MAX,
                _ => m.output = OutputSpec::Embedding { dim: 0 },
            };
            assert!(Embedder::load(&p, &m).is_err());
        }
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn concurrent_faces_share_weights_and_give_deterministic_embeddings() {
        let p = temp_model("concurrent", &crate::synthetic::tiny_embedder_model(16));
        let e = Embedder::load(&p, &manifest(16)).unwrap();
        let input = vec![123; 112 * 112 * 3];
        let want = e.embed(&input).unwrap();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let e = e.clone();
                let input = input.clone();
                std::thread::spawn(move || e.embed(&input))
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap().unwrap(), want);
        }
        let _ = std::fs::remove_file(p);
    }
}
