//! Safetensors weights read on demand.
//!
//! Tensors are read from the file when a module asks for them (positional reads, no memory
//! map: `unsafe` stays in `dac-sysmem`), so building only the point-prompt model never
//! touches the text encoder's bytes.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use candle_core::{DType, Device, Shape, Tensor};
use candle_nn::var_builder::SimpleBackend;

use crate::{Error, Result};

struct Entry {
    shape: Vec<usize>,
    start: u64,
    end: u64,
}

/// An open safetensors file: its header, and the file to read tensor bytes from.
pub struct Weights {
    file: Mutex<File>,
    entries: HashMap<String, Entry>,
    data_start: u64,
}

impl Weights {
    pub fn open(path: &Path) -> Result<Arc<Self>> {
        let mut file = File::open(path).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        let file_len = file.metadata().map_err(|e| Error::Model(format!("{}: {e}", path.display())))?.len();
        let mut len = [0u8; 8];
        file.read_exact(&mut len).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        let n = u64::from_le_bytes(len);
        if n > 100 << 20 || n.saturating_add(8) > file_len {
            return Err(Error::Model(format!("{}: not a safetensors file (header of {n} bytes)", path.display())));
        }
        let mut header = vec![0u8; n as usize];
        file.read_exact(&mut header).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        let header: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&header).map_err(|e| Error::Model(format!("{}: bad header: {e}", path.display())))?;
        let mut entries = HashMap::new();
        for (name, v) in header {
            if name == "__metadata__" {
                continue;
            }
            let dtype = match v.get("dtype").and_then(|d| d.as_str()) {
                Some("F32") => DType::F32,
                other => return Err(Error::Model(format!("{name}: unsupported dtype {other:?} (the SAM 3 checkpoint is F32)"))),
            };
            let shape: Vec<usize> = v
                .get("shape")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_u64().map(|x| x as usize)).collect())
                .unwrap_or_default();
            let offsets: Vec<u64> =
                v.get("data_offsets").and_then(|s| s.as_array()).map(|a| a.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_default();
            let [start, end] = offsets[..] else { return Err(Error::Model(format!("{name}: bad data_offsets"))) };
            let want = shape.iter().try_fold(dtype.size_in_bytes(), |a, d| a.checked_mul(*d));
            if end.checked_sub(start) != want.map(|w| w as u64) {
                return Err(Error::Model(format!("{name}: size does not match its shape")));
            }
            // the bytes must be in the file (a damaged or truncated download fails here, not later)
            if (8 + n).checked_add(end).is_none_or(|e| e > file_len) {
                return Err(Error::Model(format!("{}: tensor {name} lies outside the file (incomplete or damaged download?)", path.display())));
            }
            entries.insert(name, Entry { shape, start, end });
        }
        Ok(Arc::new(Self { file: Mutex::new(file), entries, data_start: 8 + n }))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// Tensor `name` as stored, on the CPU.
    pub fn read(&self, name: &str) -> Result<Tensor> {
        let e = self.entries.get(name).ok_or_else(|| Error::Model(format!("missing tensor `{name}`")))?;
        // checked against the file's length in `open`
        let len = usize::try_from(e.end.saturating_sub(e.start)).map_err(|_| Error::Model(format!("{name}: too large")))?;
        let mut bytes = vec![0u8; len];
        {
            let mut f = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            f.seek(SeekFrom::Start(self.data_start + e.start)).map_err(|err| Error::Model(format!("{name}: {err}")))?;
            f.read_exact(&mut bytes).map_err(|err| Error::Model(format!("{name}: {err}")))?;
        }
        let v: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
        Ok(Tensor::from_vec(v, e.shape.as_slice(), &Device::Cpu)?)
    }
}

/// [`Weights`] as a candle `VarBuilder` backend.
pub struct Backend(pub Arc<Weights>);

impl SimpleBackend for Backend {
    fn get(&self, s: Shape, name: &str, _h: candle_nn::Init, dtype: DType, dev: &Device) -> candle_core::Result<Tensor> {
        let t = self.get_unchecked(name, dtype, dev)?;
        if t.shape() != &s {
            candle_core::bail!("shape mismatch for {name}: expected {s:?}, got {:?}", t.shape())
        }
        Ok(t)
    }

    fn get_unchecked(&self, name: &str, dtype: DType, dev: &Device) -> candle_core::Result<Tensor> {
        let t = self.0.read(name).map_err(|e| candle_core::Error::Msg(e.to_string()))?;
        t.to_dtype(dtype)?.to_device(dev)
    }

    fn contains_tensor(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(name: &str, header: &str, data_len: usize) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lc-weights-{name}-{}.safetensors", std::process::id()));
        let mut b = (header.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(header.as_bytes());
        b.extend(std::iter::repeat_n(0u8, data_len));
        std::fs::write(&p, b).unwrap();
        p
    }

    #[test]
    fn tensors_must_lie_inside_the_file() {
        let ok = write("ok", r#"{"a":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]}}"#, 16);
        let w = Weights::open(&ok).unwrap();
        assert_eq!(w.read("a").unwrap().dims(), &[2, 2]);
        // a truncated download: the header promises bytes that aren't there
        let short = write("short", r#"{"a":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]}}"#, 8);
        assert!(Weights::open(&short).err().unwrap().to_string().contains("outside the file"));
        // hostile offsets: huge, or beyond u64
        let huge = write("huge", r#"{"a":{"dtype":"F32","shape":[1073741824,1073741824],"data_offsets":[0,4611686018427387904]}}"#, 16);
        assert!(Weights::open(&huge).is_err());
        let wrap = write("wrap", r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[18446744073709551612,18446744073709551616]}}"#, 16);
        assert!(Weights::open(&wrap).is_err());
        // a header longer than the file
        let p = std::env::temp_dir().join(format!("lc-weights-hdr-{}.safetensors", std::process::id()));
        std::fs::write(&p, 1000u64.to_le_bytes()).unwrap();
        assert!(Weights::open(&p).is_err());
        for f in [ok, short, huge, wrap, p] {
            let _ = std::fs::remove_file(f);
        }
    }
}
