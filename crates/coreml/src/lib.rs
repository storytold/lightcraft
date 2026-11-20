//! Run a compiled Core ML model on the CPU, the GPU or the Neural Engine.
//!
//! Core ML is the only way to reach Apple's Neural Engine: wgpu, Metal and candle never do. This crate is the
//! workspace's window on it: compile a `.mlmodel` once ([`compile`]), load the result for a set of compute units
//! ([`Model::load`]) and run single-input, single-output tensors through it ([`Model::predict`]). Core ML decides per
//! layer where it really runs; [`ComputeUnits::CpuAndNeuralEngine`] only allows the Neural Engine.
//!
//! Like `lightcraft-sysmem` this is one of the workspace's two crates allowed `unsafe` (every call into the framework,
//! all in `macos.rs`). The API is safe, every failure is an [`Error`], and on other platforms every call says
//! [`Error::Unsupported`] so callers keep their GPU / CPU path. Arrays Core ML returns are read through
//! [`layout`], which treats every number the framework reports as hostile.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod layout;
pub mod mil;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(not(target_os = "macos"))]
use unsupported as imp;

use std::path::{Path, PathBuf};

/// Why a Core ML call did not work. Never a panic: the caller runs the model elsewhere.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Core ML is not available on this platform")]
    Unsupported,
    #[error("could not compile the model: {0}")]
    Compile(String),
    #[error("could not load the model: {0}")]
    Load(String),
    #[error("{0}")]
    Input(String),
    #[error("could not write the model: {0}")]
    Build(String),
    #[error("the model's answer could not be used: {0}")]
    Output(String),
}

/// Which processors Core ML may use for a model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComputeUnits {
    /// The processor only.
    CpuOnly,
    /// The processor and the graphics card.
    CpuAndGpu,
    /// The processor and the Neural Engine (layers it cannot run go to the processor, never to the graphics card).
    CpuAndNeuralEngine,
    /// Whatever Core ML finds best.
    #[default]
    All,
}

/// A tensor: its shape and its elements in row-major order.
#[derive(Clone, Debug, PartialEq)]
pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// Whether this build can run Core ML models at all (macOS). Says nothing about the Neural Engine.
pub const fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Compile the model at `source` (a `.mlmodel` file or a `.mlpackage` folder, see [`mil`]) into `dest_dir`, as `<file name without the suffix>.mlmodelc`, and
/// return its path. Takes seconds for a large model: run it off the UI thread. A compiled model already there under
/// that name (the same source file name) is replaced.
pub fn compile(source: &Path, dest_dir: &Path) -> Result<PathBuf, Error> {
    imp::compile(source, dest_dir)
}

/// A loaded model with one input and one output, safe to share between threads (calls take turns).
pub struct Model {
    inner: imp::Loaded,
}

impl Model {
    /// Load the compiled model at `compiled` (a `.mlmodelc` folder, see [`compile`]) for `units`, running the tensor
    /// called `input` into the one called `output`.
    pub fn load(compiled: &Path, units: ComputeUnits, input: &str, output: &str) -> Result<Model, Error> {
        Ok(Model { inner: imp::Loaded::load(compiled, units, input, output)? })
    }

    /// Run one tensor of `shape` (`data` row-major) through the model.
    pub fn predict(&self, shape: &[usize], data: &[f32]) -> Result<Tensor, Error> {
        let n = layout::element_count(shape)?;
        if data.len() != n {
            return Err(Error::Input(format!("{} values for a tensor of {shape:?} ({n})", data.len())));
        }
        self.inner.predict(shape, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty folder for a test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-coreml-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Test: a missing model is an error.
    #[test]
    fn a_missing_model_is_an_error() {
        let dir = scratch("missing");
        assert!(Model::load(&dir.join("nothing.mlmodelc"), ComputeUnits::All, "input", "output").is_err());
        assert!(compile(&dir.join("nothing.mlmodel"), &dir).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Test: a file that is not a model is an error not a crash.
    #[test]
    fn a_file_that_is_not_a_model_is_an_error_not_a_crash() {
        let dir = scratch("garbage");
        let bad = dir.join("bad.mlmodel");
        std::fs::write(&bad, b"this is not a protobuf \x00\xff\xfe").unwrap();
        assert!(compile(&bad, &dir).is_err());
        // a folder that is not a compiled model
        let folder = dir.join("empty.mlmodelc");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(Model::load(&folder, ComputeUnits::CpuOnly, "input", "output").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Opt-in: `LC_COREML_MODEL=<model.mlmodel>` (one input `LC_COREML_INPUT` of `LC_COREML_SHAPE`, default x and 1,4,512,512, one
    /// output `LC_COREML_OUTPUT`) compiles the model, runs it on each kind of compute units and checks the answers agree.
    #[test]
    fn a_real_model_runs_on_every_kind_of_units() {
        let Some(path) = std::env::var_os("LC_COREML_MODEL") else {
            lightcraft_testkit::skip!(Model, "set LC_COREML_MODEL to a .mlmodel file");
            return;
        };
        if !supported() {
            lightcraft_testkit::skip!(System, "Core ML is macOS only");
            return;
        }
        let shape: Vec<usize> =
            std::env::var("LC_COREML_SHAPE").unwrap_or_else(|_| "1,4,512,512".into()).split(',').map(|s| s.trim().parse().unwrap()).collect();
        let name = |key: &str, default: &str| std::env::var(key).unwrap_or_else(|_| default.into());
        let (input_name, output_name) = (name("LC_COREML_INPUT", "x"), name("LC_COREML_OUTPUT", "output"));
        let n = layout::element_count(&shape).unwrap();
        let input: Vec<f32> = (0..n).map(|i| 0.15 + 0.1 * ((i % 512) as f32 / 512.0)).collect();
        let dir = scratch("real");
        let started = std::time::Instant::now();
        let compiled = compile(Path::new(&path), &dir).unwrap();
        println!("compiled in {:.2} s", started.elapsed().as_secs_f64());
        let mut answers = Vec::new();
        for units in [ComputeUnits::CpuOnly, ComputeUnits::CpuAndGpu, ComputeUnits::CpuAndNeuralEngine] {
            let model = Model::load(&compiled, units, &input_name, &output_name).unwrap();
            let first = model.predict(&shape, &input).unwrap();
            let mut best = f64::INFINITY;
            for _ in 0..5 {
                let t = std::time::Instant::now();
                model.predict(&shape, &input).unwrap();
                best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            }
            println!("{units:?}: {best:.1} ms per tile, output {:?}", first.shape);
            assert!(first.data.iter().all(|v| v.is_finite()), "{units:?} gave numbers that are not finite");
            answers.push(first);
        }
        let scale = answers[0].data.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-12);
        for a in &answers[1..] {
            assert_eq!(a.shape, answers[0].shape);
            let worst = a.data.iter().zip(&answers[0].data).fold(0f32, |m, (x, y)| m.max((x - y).abs())) / scale;
            println!("worst difference from the processor: {worst:.2e} of the largest value");
            assert!(worst < 2e-2);
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
