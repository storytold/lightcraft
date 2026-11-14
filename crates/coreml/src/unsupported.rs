//! Platforms without Core ML: every call says so, and the caller keeps its GPU / CPU path.

use crate::{ComputeUnits, Error, Tensor};
use std::path::{Path, PathBuf};

/// Core ML is not available here.
pub fn compile(_source: &Path, _dest_dir: &Path) -> Result<PathBuf, Error> {
    Err(Error::Unsupported)
}

pub struct Loaded;

impl Loaded {
    /// Core ML is not available here.
    pub fn load(_compiled: &Path, _units: ComputeUnits, _input: &str, _output: &str) -> Result<Loaded, Error> {
        Err(Error::Unsupported)
    }

    /// Core ML is not available here.
    pub fn predict(&self, _shape: &[usize], _data: &[f32]) -> Result<Tensor, Error> {
        Err(Error::Unsupported)
    }
}
