//! A streaming probe of an ONNX file: the graph's inputs and outputs (name, element type, shape), without
//! running or even fully loading the model.
//!
//! ONNX is protobuf. We read only the few fields we need (the producer, the opset, and the graph's input,
//! output and initializer names) and seek past everything else, so a 260 MB model costs a few kilobytes and
//! a few milliseconds. The file is hostile input: every length is checked against its parent message and
//! the file size, the number of fields read is capped, strings are bounded, and nothing indexes or unwraps.

use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const MAX_FIELDS: u32 = 5_000_000;
const MAX_STRING: u64 = 4096;
const MAX_DIMS: usize = 16;
const MAX_TENSORS: usize = 64;
/// Older exporters list every weight as a graph input too; that many are read before the weights are filtered out.
const MAX_LISTED_INPUTS: usize = 200_000;
const MAX_INITIALIZERS: usize = 1_000_000;

#[derive(Clone, Debug, PartialEq)]
pub enum Dim {
    Fixed(u64),
    /// A named or unknown size (typically the batch).
    Dynamic(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TensorInfo {
    pub name: String,
    /// ONNX element type (1 = float32).
    pub elem_type: u32,
    pub shape: Vec<Dim>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OnnxInfo {
    pub producer: String,
    /// The default-domain opset version.
    pub opset: Option<u64>,
    /// Real inputs only (weights that older exporters also list as inputs are left out).
    pub inputs: Vec<TensorInfo>,
    pub outputs: Vec<TensorInfo>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("not an ONNX model: {0}")]
    Malformed(&'static str),
    #[error("could not read the model: {0}")]
    Io(#[from] io::Error),
}

type Result<T> = std::result::Result<T, ProbeError>;

enum Wire {
    Varint(u64),
    /// A length-delimited field: its bytes are `start..end` and the reader sits at `start`.
    Len {
        start: u64,
        end: u64,
    },
    /// Fixed-width data already skipped.
    Skipped,
}

struct Walker<'a, R> {
    r: &'a mut R,
    total: u64,
    fields: u32,
}

impl<R: Read + Seek> Walker<'_, R> {
    fn pos(&mut self) -> Result<u64> {
        Ok(self.r.stream_position()?)
    }

    fn seek_to(&mut self, at: u64) -> Result<()> {
        self.r.seek(SeekFrom::Start(at))?;
        Ok(())
    }

    fn byte(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.r
            .read_exact(&mut b)
            .map_err(|e| if e.kind() == io::ErrorKind::UnexpectedEof { ProbeError::Malformed("file ends inside a field") } else { e.into() })?;
        let [x] = b;
        Ok(x)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..70u32).step_by(7) {
            let b = self.byte()?;
            if shift < 64 {
                v |= u64::from(b & 0x7f) << shift;
            }
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(ProbeError::Malformed("number too long"))
    }

    /// The next field of the message that ends at `end`, or `None` at its end.
    fn next(&mut self, end: u64) -> Result<Option<(u32, Wire)>> {
        let here = self.pos()?;
        if here >= end {
            return Ok(None);
        }
        self.fields += 1;
        if self.fields > MAX_FIELDS {
            return Err(ProbeError::Malformed("too many fields"));
        }
        let tag = self.varint()?;
        let number = u32::try_from(tag >> 3).map_err(|_| ProbeError::Malformed("field number too large"))?;
        let wire = match tag & 7 {
            0 => Wire::Varint(self.varint()?),
            1 | 5 => {
                let n = if tag & 7 == 1 { 8 } else { 4 };
                let at = self.pos()?.checked_add(n).ok_or(ProbeError::Malformed("offset overflow"))?;
                if at > end {
                    return Err(ProbeError::Malformed("field runs past its message"));
                }
                self.seek_to(at)?;
                Wire::Skipped
            }
            2 => {
                let len = self.varint()?;
                let start = self.pos()?;
                let stop = start.checked_add(len).ok_or(ProbeError::Malformed("offset overflow"))?;
                if stop > end || stop > self.total {
                    return Err(ProbeError::Malformed("field runs past its message"));
                }
                Wire::Len { start, end: stop }
            }
            _ => return Err(ProbeError::Malformed("unsupported field type")),
        };
        Ok(Some((number, wire)))
    }

    /// A bounded UTF-8 string occupying `start..end` (empty when it is longer than we care about).
    fn string(&mut self, start: u64, end: u64) -> Result<String> {
        let len = end.saturating_sub(start);
        if len > MAX_STRING {
            return Ok(String::new());
        }
        let mut buf = Vec::new();
        self.r.by_ref().take(len).read_to_end(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

fn dim<R: Read + Seek>(w: &mut Walker<R>, end: u64) -> Result<Option<Dim>> {
    let mut out = None;
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (1, Wire::Varint(v)) => out = Some(Dim::Fixed(v)),
            (2, Wire::Len { start, end: e }) => {
                out = Some(Dim::Dynamic(w.string(start, e)?));
                w.seek_to(e)?;
            }
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(out)
}

fn shape<R: Read + Seek>(w: &mut Walker<R>, end: u64) -> Result<Vec<Dim>> {
    let mut dims = Vec::new();
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (1, Wire::Len { end: e, .. }) => {
                if dims.len() >= MAX_DIMS {
                    return Err(ProbeError::Malformed("too many dimensions"));
                }
                dims.push(dim(w, e)?.unwrap_or(Dim::Dynamic(String::new())));
                w.seek_to(e)?;
            }
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(dims)
}

fn tensor_type<R: Read + Seek>(w: &mut Walker<R>, end: u64, info: &mut TensorInfo) -> Result<()> {
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (1, Wire::Varint(v)) => info.elem_type = u32::try_from(v).unwrap_or(0),
            (2, Wire::Len { end: e, .. }) => {
                info.shape = shape(w, e)?;
                w.seek_to(e)?;
            }
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(())
}

fn type_proto<R: Read + Seek>(w: &mut Walker<R>, end: u64, info: &mut TensorInfo) -> Result<()> {
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (1, Wire::Len { end: e, .. }) => {
                tensor_type(w, e, info)?;
                w.seek_to(e)?;
            }
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(())
}

fn value_info<R: Read + Seek>(w: &mut Walker<R>, end: u64) -> Result<TensorInfo> {
    let mut info = TensorInfo { name: String::new(), elem_type: 0, shape: Vec::new() };
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (1, Wire::Len { start, end: e }) => {
                info.name = w.string(start, e)?;
                w.seek_to(e)?;
            }
            (2, Wire::Len { end: e, .. }) => {
                type_proto(w, e, &mut info)?;
                w.seek_to(e)?;
            }
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(info)
}

/// The `name` (field 8) of a weight tensor; its bulk data is skipped, never read.
fn tensor_name<R: Read + Seek>(w: &mut Walker<R>, end: u64) -> Result<Option<String>> {
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (8, Wire::Len { start, end: e }) => return Ok(Some(w.string(start, e)?)),
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(None)
}

fn graph<R: Read + Seek>(w: &mut Walker<R>, end: u64, info: &mut OnnxInfo, weights: &mut HashSet<String>) -> Result<()> {
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (5, Wire::Len { end: e, .. }) => {
                if weights.len() < MAX_INITIALIZERS
                    && let Some(name) = tensor_name(w, e)?
                {
                    weights.insert(name);
                }
                w.seek_to(e)?;
            }
            (11, Wire::Len { end: e, .. }) => {
                if info.inputs.len() >= MAX_LISTED_INPUTS {
                    return Err(ProbeError::Malformed("too many inputs"));
                }
                info.inputs.push(value_info(w, e)?);
                w.seek_to(e)?;
            }
            (12, Wire::Len { end: e, .. }) => {
                if info.outputs.len() >= MAX_TENSORS {
                    return Err(ProbeError::Malformed("too many outputs"));
                }
                info.outputs.push(value_info(w, e)?);
                w.seek_to(e)?;
            }
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(())
}

fn opset<R: Read + Seek>(w: &mut Walker<R>, end: u64) -> Result<Option<u64>> {
    let (mut domain, mut version) = (String::new(), None);
    while let Some((n, f)) = w.next(end)? {
        match (n, f) {
            (1, Wire::Len { start, end: e }) => {
                domain = w.string(start, e)?;
                w.seek_to(e)?;
            }
            (2, Wire::Varint(v)) => version = Some(v),
            (_, Wire::Len { end: e, .. }) => w.seek_to(e)?,
            _ => {}
        }
    }
    Ok(if domain.is_empty() || domain == "ai.onnx" { version } else { None })
}

/// Read an ONNX model's graph inputs and outputs from any seekable source.
pub fn probe_reader<R: Read + Seek>(r: &mut R) -> Result<OnnxInfo> {
    let total = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    if total == 0 {
        return Err(ProbeError::Malformed("the file is empty"));
    }
    let mut w = Walker { r, total, fields: 0 };
    let (mut info, mut weights, mut saw_graph) = (OnnxInfo::default(), HashSet::new(), false);
    while let Some((n, f)) = w.next(total)? {
        match (n, f) {
            (2, Wire::Len { start, end }) => {
                info.producer = w.string(start, end)?;
                w.seek_to(end)?;
            }
            (7, Wire::Len { end, .. }) => {
                saw_graph = true;
                graph(&mut w, end, &mut info, &mut weights)?;
                w.seek_to(end)?;
            }
            (8, Wire::Len { end, .. }) => {
                if let Some(v) = opset(&mut w, end)? {
                    info.opset = Some(v);
                }
                w.seek_to(end)?;
            }
            (_, Wire::Len { end, .. }) => w.seek_to(end)?,
            _ => {}
        }
    }
    if !saw_graph {
        return Err(ProbeError::Malformed("no graph"));
    }
    info.inputs.retain(|t| !weights.contains(&t.name));
    if info.inputs.len() > MAX_TENSORS {
        return Err(ProbeError::Malformed("too many inputs"));
    }
    if info.inputs.is_empty() || info.outputs.is_empty() {
        return Err(ProbeError::Malformed("the graph has no inputs or no outputs"));
    }
    Ok(info)
}

/// Read an ONNX file's graph inputs and outputs.
pub fn probe_path(path: &Path) -> Result<OnnxInfo> {
    probe_reader(&mut io::BufReader::new(std::fs::File::open(path)?))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn len_field(n: u64, body: &[u8], out: &mut Vec<u8>) {
        varint(n << 3 | 2, out);
        varint(body.len() as u64, out);
        out.extend_from_slice(body);
    }
    fn varint_field(n: u64, v: u64, out: &mut Vec<u8>) {
        varint(n << 3, out);
        varint(v, out);
    }
    fn value_info_bytes(name: &str, elem: u64, dims: &[std::result::Result<u64, &str>]) -> Vec<u8> {
        let mut shape = Vec::new();
        for d in dims {
            let mut dim = Vec::new();
            match d {
                Ok(v) => varint_field(1, *v, &mut dim),
                Err(p) => len_field(2, p.as_bytes(), &mut dim),
            }
            len_field(1, &dim, &mut shape);
        }
        let mut tensor = Vec::new();
        varint_field(1, elem, &mut tensor);
        len_field(2, &shape, &mut tensor);
        let mut ty = Vec::new();
        len_field(1, &tensor, &mut ty);
        let mut vi = Vec::new();
        len_field(1, name.as_bytes(), &mut vi);
        len_field(2, &ty, &mut vi);
        vi
    }

    /// A small valid model: input `data` [N, 3, 112, 112] float, an old-style weight also listed as an
    /// input, a weight with bulk data, output `emb` [N, 512], ai.onnx opset 17.
    pub(crate) fn model_bytes(output_dim: u64) -> Vec<u8> {
        let mut graph = Vec::new();
        len_field(11, &value_info_bytes("data", 1, &[Err("N"), Ok(3), Ok(112), Ok(112)]), &mut graph);
        len_field(11, &value_info_bytes("w", 1, &[Ok(4)]), &mut graph);
        let mut tensor = Vec::new();
        varint_field(1, 4, &mut tensor);
        len_field(8, b"w", &mut tensor);
        len_field(9, &[7u8; 5000], &mut tensor);
        len_field(5, &tensor, &mut graph);
        len_field(12, &value_info_bytes("emb", 1, &[Err("N"), Ok(output_dim)]), &mut graph);
        let mut ops = Vec::new();
        len_field(1, b"", &mut ops);
        varint_field(2, 17, &mut ops);
        let mut model = Vec::new();
        varint_field(1, 8, &mut model);
        len_field(2, b"pytorch", &mut model);
        len_field(7, &graph, &mut model);
        len_field(8, &ops, &mut model);
        model
    }

    #[test]
    fn reads_inputs_outputs_and_skips_weights() {
        let info = probe_reader(&mut Cursor::new(model_bytes(512))).unwrap();
        assert_eq!(info.producer, "pytorch");
        assert_eq!(info.opset, Some(17));
        assert_eq!(info.inputs.len(), 1, "the weight listed as an input is left out");
        assert_eq!(info.inputs[0].name, "data");
        assert_eq!(info.inputs[0].elem_type, 1);
        assert_eq!(info.inputs[0].shape, vec![Dim::Dynamic("N".into()), Dim::Fixed(3), Dim::Fixed(112), Dim::Fixed(112)]);
        assert_eq!(info.outputs[0].shape, vec![Dim::Dynamic("N".into()), Dim::Fixed(512)]);
    }

    #[test]
    fn old_style_files_listing_hundreds_of_weights_as_inputs_still_probe() {
        let mut graph = Vec::new();
        len_field(11, &value_info_bytes("data", 1, &[Err("N"), Ok(3), Ok(112), Ok(112)]), &mut graph);
        for i in 0..500 {
            let name = format!("weight{i}");
            len_field(11, &value_info_bytes(&name, 1, &[Ok(4)]), &mut graph);
            let mut tensor = Vec::new();
            len_field(8, name.as_bytes(), &mut tensor);
            len_field(5, &tensor, &mut graph);
        }
        len_field(12, &value_info_bytes("emb", 1, &[Err("N"), Ok(128)]), &mut graph);
        let mut m = Vec::new();
        len_field(7, &graph, &mut m);
        let info = probe_reader(&mut Cursor::new(m)).unwrap();
        assert_eq!(info.inputs.len(), 1);
        assert_eq!(info.inputs[0].name, "data");
    }

    #[test]
    fn many_real_inputs_are_refused() {
        let mut graph = Vec::new();
        for i in 0..70 {
            len_field(11, &value_info_bytes(&format!("in{i}"), 1, &[Ok(4)]), &mut graph);
        }
        len_field(12, &value_info_bytes("emb", 1, &[Ok(4)]), &mut graph);
        let mut m = Vec::new();
        len_field(7, &graph, &mut m);
        assert!(probe_reader(&mut Cursor::new(m)).is_err());
    }

    #[test]
    fn truncated_at_every_length_never_panics() {
        let bytes = model_bytes(128);
        for n in 0..bytes.len() {
            let _ = probe_reader(&mut Cursor::new(bytes[..n].to_vec()));
        }
        assert!(probe_reader(&mut Cursor::new(bytes)).is_ok());
    }

    #[test]
    fn hostile_files_are_errors() {
        let mut lying = Vec::new();
        varint(7 << 3 | 2, &mut lying);
        varint(1 << 40, &mut lying); // a graph 1 TB long in a 10-byte file
        let mut overflow = Vec::new();
        varint(7 << 3 | 2, &mut overflow);
        varint(u64::MAX, &mut overflow);
        let mut long_number = vec![0x08];
        long_number.extend_from_slice(&[0xff; 11]);
        for (what, bytes) in [
            ("empty", Vec::new()),
            ("zeros", vec![0u8; 64]),
            ("text", b"this is not an onnx model at all".to_vec()),
            ("lying length", lying),
            ("overflowing length", overflow),
            ("endless number", long_number),
            ("no graph", {
                let mut m = Vec::new();
                len_field(2, b"x", &mut m);
                m
            }),
            ("graph without io", {
                let mut m = Vec::new();
                len_field(7, &[], &mut m);
                m
            }),
        ] {
            assert!(probe_reader(&mut Cursor::new(bytes)).is_err(), "{what}");
        }
    }

    #[test]
    fn too_many_dimensions_or_tensors_are_errors() {
        let dims: Vec<std::result::Result<u64, &str>> = (0..40).map(Ok).collect();
        let mut graph = Vec::new();
        len_field(11, &value_info_bytes("data", 1, &dims), &mut graph);
        len_field(12, &value_info_bytes("out", 1, &[Ok(1)]), &mut graph);
        let mut m = Vec::new();
        len_field(7, &graph, &mut m);
        assert!(probe_reader(&mut Cursor::new(m)).is_err());
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..400)) {
            let _ = probe_reader(&mut Cursor::new(bytes));
        }

        #[test]
        fn a_valid_model_with_flipped_bytes_never_panics(flips in proptest::collection::vec((0usize..400, proptest::prelude::any::<u8>()), 1..6)) {
            let mut bytes = model_bytes(512);
            for (at, v) in flips {
                if let Some(b) = bytes.get_mut(at) {
                    *b = v;
                }
            }
            let _ = probe_reader(&mut Cursor::new(bytes));
        }
    }
}
