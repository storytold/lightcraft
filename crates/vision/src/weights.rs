//! Safetensors weights read on demand (the same approach as `lightcraft-segment`'s reader).
//!
//! Tensors are read from the file when a module asks for them (positional reads, no memory
//! map: `unsafe` stays in `lightcraft-sysmem`), so loading only the image tower never touches
//! the text tower's bytes. [`Weights::read_rows`] reads single rows of a table (the 256k-token
//! embedding table of the text tower stays on disk: ~790 MB not held in memory).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use candle_core::{DType, Device, Shape, Tensor};
use candle_nn::var_builder::SimpleBackend;

use crate::{Error, Result};

/// Largest safetensors header read.
const MAX_HEADER: u64 = 100 << 20;
/// Most rows read at once.
const MAX_ROWS: usize = 1 << 16;

struct Entry {
    dtype: DType,
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
        let err = |e: std::io::Error| Error::Model(format!("{}: {e}", path.display()));
        let mut file = File::open(path).map_err(err)?;
        let file_len = file.metadata().map_err(err)?.len();
        let mut len = [0u8; 8];
        file.read_exact(&mut len).map_err(err)?;
        let n = u64::from_le_bytes(len);
        if n > MAX_HEADER || n.saturating_add(8) > file_len {
            return Err(Error::Model(format!("{}: not a safetensors file (header of {n} bytes)", path.display())));
        }
        let mut header = vec![0u8; n as usize];
        file.read_exact(&mut header).map_err(err)?;
        let header: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&header).map_err(|e| Error::Model(format!("{}: bad header: {e}", path.display())))?;
        let mut entries = HashMap::new();
        for (name, v) in header {
            if name == "__metadata__" {
                continue;
            }
            let dtype = match v.get("dtype").and_then(|d| d.as_str()) {
                Some("F32") => DType::F32,
                Some("F16") => DType::F16,
                Some("BF16") => DType::BF16,
                // (CLIP checkpoints carry `position_ids` tables, never read)
                Some("I64") => DType::I64,
                other => return Err(Error::Model(format!("{name}: unsupported dtype {other:?}"))),
            };
            let shape: Option<Vec<usize>> = v
                .get("shape")
                .and_then(|s| s.as_array())
                .map(|a| a.iter().map(|x| x.as_u64().and_then(|x| usize::try_from(x).ok())).collect())
                .unwrap_or(Some(Vec::new()));
            let Some(shape) = shape else { return Err(Error::Model(format!("{name}: bad shape"))) };
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
            entries.insert(name, Entry { dtype, shape, start, end });
        }
        Ok(Arc::new(Self { file: Mutex::new(file), entries, data_start: 8 + n }))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.entries.get(name).map(|e| e.shape.as_slice())
    }

    fn entry(&self, name: &str) -> Result<&Entry> {
        self.entries.get(name).ok_or_else(|| Error::Model(format!("missing tensor `{name}`")))
    }

    fn read_at(&self, name: &str, offset: u64, bytes: &mut [u8]) -> Result<()> {
        let mut f = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        f.seek(SeekFrom::Start(self.data_start.saturating_add(offset))).map_err(|err| Error::Model(format!("{name}: {err}")))?;
        f.read_exact(bytes).map_err(|err| Error::Model(format!("{name}: {err}")))
    }

    /// Tensor `name` as stored, on the CPU.
    pub fn read(&self, name: &str) -> Result<Tensor> {
        let e = self.entry(name)?;
        // checked against the file's length in `open`
        let len = usize::try_from(e.end.saturating_sub(e.start)).map_err(|_| Error::Model(format!("{name}: too large")))?;
        let mut bytes = vec![0u8; len];
        self.read_at(name, e.start, &mut bytes)?;
        Ok(Tensor::from_raw_buffer(&bytes, e.dtype, &e.shape, &Device::Cpu)?)
    }

    /// Rows `ids` of the 2-D tensor `name` as `[ids.len(), width]`, on the CPU.
    pub fn read_rows(&self, name: &str, ids: &[u32]) -> Result<Tensor> {
        let e = self.entry(name)?;
        let [rows, width] = e.shape[..] else { return Err(Error::Model(format!("{name}: not a table"))) };
        if ids.len() > MAX_ROWS {
            return Err(Error::Model(format!("{name}: too many rows at once")));
        }
        let row_bytes = width.saturating_mul(e.dtype.size_in_bytes());
        let mut bytes = vec![0u8; ids.len().saturating_mul(row_bytes)];
        for (i, &id) in ids.iter().enumerate() {
            let id = id as usize;
            if id >= rows {
                return Err(Error::Model(format!("{name}: row {id} is outside the table of {rows}")));
            }
            let off = e.start.saturating_add((id as u64).saturating_mul(row_bytes as u64));
            let slot = bytes.get_mut(i * row_bytes..(i + 1) * row_bytes).ok_or_else(|| Error::Model(format!("{name}: row buffer")))?;
            self.read_at(name, off, slot)?;
        }
        Ok(Tensor::from_raw_buffer(&bytes, e.dtype, &[ids.len(), width], &Device::Cpu)?)
    }
}

impl crate::accel::TensorSource for Weights {
    /// A whole tensor as f32 (F16 / BF16 converted), with its shape.
    fn read(&self, name: &str) -> std::result::Result<(Vec<usize>, Vec<f32>), String> {
        let e = self.entry(name).map_err(|e| e.to_string())?;
        if e.dtype == DType::I64 {
            return Err(format!("{name}: not a float tensor"));
        }
        let shape = e.shape.clone();
        let t = Weights::read(self, name).map_err(|e| e.to_string())?;
        let v = t.to_dtype(DType::F32).and_then(|t| t.flatten_all()).and_then(|t| t.to_vec1::<f32>()).map_err(|e| format!("{name}: {e}"))?;
        Ok((shape, v))
    }
}

/// [`Weights`] as a candle `VarBuilder` backend.
pub struct Backend(pub Arc<Weights>);

impl SimpleBackend for Backend {
    fn get(&self, s: Shape, name: &str, _h: candle_nn::Init, dtype: DType, dev: &Device) -> candle_core::Result<Tensor> {
        // check the shape before reading the bytes
        match self.0.shape(name) {
            Some(have) if have != s.dims() => candle_core::bail!("shape mismatch for {name}: expected {s:?}, got {have:?}"),
            _ => {}
        }
        self.get_unchecked(name, dtype, dev)
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

    pub(crate) fn write(name: &str, header: &str, data: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("lc-vision-weights-{name}-{}.safetensors", std::process::id()));
        let mut b = (header.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(header.as_bytes());
        b.extend_from_slice(data);
        std::fs::write(&p, b).unwrap();
        p
    }

    #[test]
    fn tensors_must_lie_inside_the_file() {
        let ok = write("ok", r#"{"a":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]}}"#, &[0; 16]);
        let w = Weights::open(&ok).unwrap();
        assert_eq!(w.read("a").unwrap().dims(), &[2, 2]);
        assert!(w.read("b").is_err());
        let short = write("short", r#"{"a":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]}}"#, &[0; 8]);
        assert!(Weights::open(&short).err().unwrap().to_string().contains("outside the file"));
        let huge = write("huge", r#"{"a":{"dtype":"F32","shape":[1073741824,1073741824],"data_offsets":[0,4611686018427387904]}}"#, &[0; 16]);
        assert!(Weights::open(&huge).is_err());
        let wrap = write("wrap", r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[18446744073709551612,18446744073709551616]}}"#, &[0; 16]);
        assert!(Weights::open(&wrap).is_err());
        let neg = write("neg", r#"{"a":{"dtype":"F32","shape":[-1],"data_offsets":[0,4]}}"#, &[0; 4]);
        assert!(Weights::open(&neg).is_err());
        let int = write("int", r#"{"a":{"dtype":"I64","shape":[1],"data_offsets":[0,8]}}"#, &[0; 8]);
        assert!(Weights::open(&int).is_ok(), "integer tables are listed");
        let int = write("int", r#"{"a":{"dtype":"I64","shape":[2],"data_offsets":[0,8]}}"#, &[0; 8]);
        assert!(Weights::open(&int).is_err(), "and their size checked");
        let odd = write("odd", r#"{"a":{"dtype":"F8_E4M3","shape":[1],"data_offsets":[0,1]}}"#, &[0; 1]);
        assert!(Weights::open(&odd).is_err());
        let p = std::env::temp_dir().join(format!("lc-vision-weights-hdr-{}.safetensors", std::process::id()));
        std::fs::write(&p, 1000u64.to_le_bytes()).unwrap();
        assert!(Weights::open(&p).is_err());
        std::fs::write(&p, [1u8, 2]).unwrap();
        assert!(Weights::open(&p).is_err());
        for f in [ok, short, huge, wrap, neg, int, odd, p] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn rows_are_read_by_index_and_bounded() {
        let data: Vec<u8> = (0..6).flat_map(|v| (v as f32).to_le_bytes()).collect();
        let p = write("rows", r#"{"t":{"dtype":"F32","shape":[3,2],"data_offsets":[0,24]}}"#, &data);
        let w = Weights::open(&p).unwrap();
        let r = w.read_rows("t", &[2, 0, 2]).unwrap();
        assert_eq!(r.flatten_all().unwrap().to_vec1::<f32>().unwrap(), vec![4.0, 5.0, 0.0, 1.0, 4.0, 5.0]);
        assert!(w.read_rows("t", &[3]).is_err());
        assert!(w.read_rows("x", &[0]).is_err());
        let _ = std::fs::remove_file(p);
    }
}
