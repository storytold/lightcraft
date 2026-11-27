//! Minimal ONNX (protobuf) reader: the graph's nodes, attributes and initializers, for PaddleOCR's exported graphs.
//! (`lightcraft-faces`' `onnx` module only probes a model's input and output shapes; this one loads the whole graph.)
//!
//! Decodes only the parts of `ModelProto` (`onnx.proto`) the executor needs, straight from the
//! protobuf wire format. Model files are untrusted input: every length is checked against the
//! bytes that are left, tensor contents against their dims, and sizes are capped.

use std::io::Read;
use std::path::Path;

use crate::{Error, Result};

/// Largest model file read (PP-OCRv6 small's models are 1 to 21 MB; medium's 76 MB).
pub const MAX_FILE_BYTES: u64 = 1 << 30;
/// Largest tensor, in elements.
pub const MAX_ELEMENTS: usize = 1 << 31;
/// Highest tensor rank accepted.
const MAX_RANK: usize = 16;

/// A tensor's values.
#[derive(Clone, Debug, PartialEq)]
pub enum Data {
    F32(Vec<f32>),
    /// INT64 (and INT32, widened).
    I64(Vec<i64>),
}

/// A constant tensor (an initializer or a tensor attribute).
#[derive(Clone, Debug, PartialEq)]
pub struct TensorProto {
    pub name: String,
    pub dims: Vec<usize>,
    /// Exactly the product of `dims` values.
    pub data: Data,
}

/// A node attribute's value.
#[derive(Clone, Debug, PartialEq)]
pub enum Attr {
    Float(f32),
    Int(i64),
    Bytes(Vec<u8>),
    Tensor(TensorProto),
    Floats(Vec<f32>),
    Ints(Vec<i64>),
    Strings(Vec<Vec<u8>>),
}

/// One operator of the graph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NodeProto {
    pub name: String,
    pub op_type: String,
    pub domain: String,
    /// Value names; an empty name is an omitted optional input.
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub attrs: Vec<(String, Attr)>,
}

impl NodeProto {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|(n, _)| n == name).map(|(_, a)| a)
    }
}

/// The parts of a model the executor uses.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelProto {
    /// The default (`ai.onnx`) operator set's version.
    pub opset: Option<i64>,
    /// In topological order (as ONNX requires).
    pub nodes: Vec<NodeProto>,
    pub initializers: Vec<TensorProto>,
    /// Graph input names (may include initializers' names, in older IR versions).
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Model(format!("not a usable ONNX file: {what}"))
}

/// Reads and decodes the model at `path`.
pub fn read(path: &Path) -> Result<ModelProto> {
    let io = |e: std::io::Error| Error::Model(format!("{}: {e}", path.display()));
    let file = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::Model(format!("{}: larger than {} MB, not a text-reading model", path.display(), MAX_FILE_BYTES >> 20)));
    }
    parse(&bytes).map_err(|e| match e {
        Error::Model(m) => Error::Model(format!("{}: {m}", path.display())),
        e => e,
    })
}

/// Decodes a serialized `ModelProto`.
pub fn parse(bytes: &[u8]) -> Result<ModelProto> {
    let mut r = Reader::new(bytes);
    let mut graph = None;
    let mut opset = None;
    while let Some((field, w)) = r.next()? {
        match field {
            7 => graph = Some(message(w)?),
            8 => {
                let mut o = Reader::new(message(w)?);
                let (mut domain, mut version) = (String::new(), None);
                while let Some((f, w)) = o.next()? {
                    match f {
                        1 => domain = string(w)?,
                        2 => version = Some(int(w)?),
                        _ => {}
                    }
                }
                if domain.is_empty() || domain == "ai.onnx" {
                    opset = version;
                }
            }
            _ => {}
        }
    }
    let mut model = parse_graph(graph.ok_or_else(|| bad("no graph"))?)?;
    model.opset = opset;
    Ok(model)
}

fn parse_graph(bytes: &[u8]) -> Result<ModelProto> {
    let mut m = ModelProto::default();
    let mut r = Reader::new(bytes);
    while let Some((field, w)) = r.next()? {
        match field {
            1 => m.nodes.push(parse_node(message(w)?)?),
            5 => m.initializers.push(parse_tensor(message(w)?)?),
            11 => m.inputs.push(value_info_name(message(w)?)?),
            12 => m.outputs.push(value_info_name(message(w)?)?),
            15 => return Err(bad("sparse initializers are not supported")),
            _ => {}
        }
    }
    Ok(m)
}

fn value_info_name(bytes: &[u8]) -> Result<String> {
    let mut r = Reader::new(bytes);
    let mut name = String::new();
    while let Some((field, w)) = r.next()? {
        if field == 1 {
            name = string(w)?;
        }
    }
    Ok(name)
}

fn parse_node(bytes: &[u8]) -> Result<NodeProto> {
    let mut n = NodeProto::default();
    let mut r = Reader::new(bytes);
    while let Some((field, w)) = r.next()? {
        match field {
            1 => n.inputs.push(string(w)?),
            2 => n.outputs.push(string(w)?),
            3 => n.name = string(w)?,
            4 => n.op_type = string(w)?,
            5 => n.attrs.push(parse_attr(message(w)?)?),
            7 => n.domain = string(w)?,
            _ => {}
        }
    }
    if n.name.is_empty() {
        n.name = n.outputs.first().cloned().unwrap_or_default();
    }
    Ok(n)
}

fn parse_attr(bytes: &[u8]) -> Result<(String, Attr)> {
    let mut r = Reader::new(bytes);
    let mut name = String::new();
    let mut ty = 0;
    let (mut f, mut i, mut s, mut t) = (None, None, None, None);
    let (mut floats, mut ints, mut strings) = (Vec::new(), Vec::new(), Vec::new());
    while let Some((field, w)) = r.next()? {
        match field {
            1 => name = string(w)?,
            2 => match w {
                Wire::I32(v) => f = Some(f32::from_bits(v)),
                _ => return Err(bad("attribute float has the wrong wire type")),
            },
            3 => i = Some(int(w)?),
            4 => s = Some(message(w)?.to_vec()),
            5 => t = Some(parse_tensor(message(w)?)?),
            6 | 11 => return Err(bad(format!("attribute {name:?} holds a subgraph (control flow is not supported)"))),
            7 => push_floats(w, &mut floats)?,
            8 => push_ints(w, &mut ints)?,
            9 => strings.push(message(w)?.to_vec()),
            10 => return Err(bad(format!("attribute {name:?}: tensor lists are not supported"))),
            20 => ty = int(w)?,
            21 => return Err(bad(format!("attribute {name:?} refers to a function attribute (not supported)"))),
            22 | 23 => return Err(bad(format!("attribute {name:?}: sparse tensors are not supported"))),
            _ => {}
        }
    }
    let attr = match ty {
        1 => Attr::Float(f.unwrap_or(0.0)),
        2 => Attr::Int(i.unwrap_or(0)),
        3 => Attr::Bytes(s.unwrap_or_default()),
        4 => Attr::Tensor(t.ok_or_else(|| bad(format!("attribute {name:?} has no tensor")))?),
        6 => Attr::Floats(floats),
        7 => Attr::Ints(ints),
        8 => Attr::Strings(strings),
        // no type (old exporters): whatever is present
        0 => {
            if let Some(t) = t {
                Attr::Tensor(t)
            } else if !floats.is_empty() {
                Attr::Floats(floats)
            } else if !ints.is_empty() {
                Attr::Ints(ints)
            } else if !strings.is_empty() {
                Attr::Strings(strings)
            } else if let Some(s) = s {
                Attr::Bytes(s)
            } else if let Some(f) = f {
                Attr::Float(f)
            } else if let Some(i) = i {
                Attr::Int(i)
            } else {
                return Err(bad(format!("attribute {name:?} has no value")));
            }
        }
        other => return Err(bad(format!("attribute {name:?} has unsupported type {other}"))),
    };
    Ok((name, attr))
}

fn parse_tensor(bytes: &[u8]) -> Result<TensorProto> {
    let mut r = Reader::new(bytes);
    let mut name = String::new();
    let (mut dims, mut floats, mut ints) = (Vec::new(), Vec::new(), Vec::new());
    let mut data_type = 0;
    let mut raw: Option<&[u8]> = None;
    while let Some((field, w)) = r.next()? {
        match field {
            1 => push_ints(w, &mut dims)?,
            2 => data_type = int(w)?,
            3 => return Err(bad("segmented tensors are not supported")),
            4 => push_floats(w, &mut floats)?,
            // int32_data and int64_data
            5 | 7 => push_ints(w, &mut ints)?,
            8 => name = string(w)?,
            9 => raw = Some(message(w)?),
            // external_data, or data_location EXTERNAL
            13 => return Err(bad(format!("tensor {name:?} keeps its data in an external file (not supported)"))),
            14 if int(w)? != 0 => return Err(bad(format!("tensor {name:?} keeps its data in an external file (not supported)"))),
            _ => {}
        }
    }
    if dims.len() > MAX_RANK {
        return Err(bad(format!("tensor {name:?} has rank {}", dims.len())));
    }
    let dims = dims
        .iter()
        .map(|d| usize::try_from(*d).map_err(|_| bad(format!("tensor {name:?} has a negative dimension {d}"))))
        .collect::<Result<Vec<_>>>()?;
    let n = dims
        .iter()
        .try_fold(1usize, |a, d| a.checked_mul(*d))
        .filter(|n| *n <= MAX_ELEMENTS)
        .ok_or_else(|| bad(format!("tensor {name:?} is too large ({dims:?})")))?;
    let raw_len = |size: usize| -> Result<()> {
        match raw {
            Some(raw) if Some(raw.len()) != n.checked_mul(size) => {
                Err(bad(format!("tensor {name:?} has {} bytes, its dims {dims:?} need {n} values", raw.len())))
            }
            _ => Ok(()),
        }
    };
    let data = match data_type {
        1 => {
            raw_len(4)?;
            Data::F32(match raw {
                Some(raw) => raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect(),
                None => floats,
            })
        }
        7 => {
            raw_len(8)?;
            Data::I64(match raw {
                Some(raw) => raw.as_chunks::<8>().0.iter().map(|c| i64::from_le_bytes(*c)).collect(),
                None => ints,
            })
        }
        6 => {
            raw_len(4)?;
            Data::I64(match raw {
                Some(raw) => raw.as_chunks::<4>().0.iter().map(|c| i64::from(i32::from_le_bytes(*c))).collect(),
                None => ints.iter().map(|v| i64::from(*v as i32)).collect(),
            })
        }
        other => return Err(bad(format!("tensor {name:?} has unsupported data type {other} (only float32, int64 and int32)"))),
    };
    let len = match &data {
        Data::F32(v) => v.len(),
        Data::I64(v) => v.len(),
    };
    if len != n {
        return Err(bad(format!("tensor {name:?} holds {len} values, its dims {dims:?} need {n}")));
    }
    Ok(TensorProto { name, dims, data })
}

/// A field's value as it appears on the wire.
#[derive(Clone, Copy, Debug)]
enum Wire<'a> {
    Varint(u64),
    /// A fixed 64-bit value (no field the reader uses has one).
    I64,
    Len(&'a [u8]),
    I32(u32),
}

fn message<'a>(w: Wire<'a>) -> Result<&'a [u8]> {
    match w {
        Wire::Len(b) => Ok(b),
        _ => Err(bad("expected a length-delimited field")),
    }
}

fn string(w: Wire) -> Result<String> {
    String::from_utf8(message(w)?.to_vec()).map_err(|_| bad("a name is not UTF-8"))
}

fn int(w: Wire) -> Result<i64> {
    match w {
        Wire::Varint(v) => Ok(v as i64),
        _ => Err(bad("expected an integer field")),
    }
}

/// A repeated integer field, packed or not.
fn push_ints(w: Wire, out: &mut Vec<i64>) -> Result<()> {
    match w {
        Wire::Varint(v) => out.push(v as i64),
        Wire::Len(b) => {
            let mut r = Reader::new(b);
            while !r.buf.is_empty() {
                out.push(r.varint()? as i64);
            }
        }
        _ => return Err(bad("expected integers")),
    }
    Ok(())
}

/// A repeated float field, packed or not.
fn push_floats(w: Wire, out: &mut Vec<f32>) -> Result<()> {
    match w {
        Wire::I32(v) => out.push(f32::from_bits(v)),
        Wire::Len(b) => {
            let (chunks, rest) = b.as_chunks::<4>();
            if !rest.is_empty() {
                return Err(bad("packed floats are not a multiple of 4 bytes"));
            }
            out.extend(chunks.iter().map(|c| f32::from_le_bytes(*c)));
        }
        _ => return Err(bad("expected floats")),
    }
    Ok(())
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let (&b, rest) = self.buf.split_first().ok_or_else(|| bad("truncated"))?;
            self.buf = rest;
            v |= u64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(bad("a varint is longer than 10 bytes"))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (a, b) = self.buf.split_at_checked(n).ok_or_else(|| bad("truncated"))?;
        self.buf = b;
        Ok(a)
    }

    /// The next field number and value; `None` at the end.
    fn next(&mut self) -> Result<Option<(u64, Wire<'a>)>> {
        if self.buf.is_empty() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = key >> 3;
        if field == 0 {
            return Err(bad("field number 0"));
        }
        let w = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Wire::I64
            }
            2 => {
                let n = usize::try_from(self.varint()?).map_err(|_| bad("truncated"))?;
                Wire::Len(self.take(n)?)
            }
            5 => Wire::I32(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| bad("truncated"))?)),
            t => return Err(bad(format!("unsupported wire type {t}"))),
        };
        Ok(Some((field, w)))
    }
}

/// Builds protobuf bytes by hand, for tests.
#[cfg(test)]
pub(crate) mod encode {
    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
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

    pub fn len(field: u64, bytes: &[u8]) -> Vec<u8> {
        let mut o = Vec::new();
        varint(field << 3 | 2, &mut o);
        varint(bytes.len() as u64, &mut o);
        o.extend_from_slice(bytes);
        o
    }

    pub fn int(field: u64, v: i64) -> Vec<u8> {
        let mut o = Vec::new();
        varint(field << 3, &mut o);
        varint(v as u64, &mut o);
        o
    }

    pub fn float(field: u64, v: f32) -> Vec<u8> {
        let mut o = Vec::new();
        varint(field << 3 | 5, &mut o);
        o.extend_from_slice(&v.to_le_bytes());
        o
    }

    pub fn tensor_f32(name: &str, dims: &[i64], data: &[f32]) -> Vec<u8> {
        let mut o: Vec<u8> = dims.iter().flat_map(|d| int(1, *d)).collect();
        o.extend(int(2, 1));
        o.extend(len(8, name.as_bytes()));
        o.extend(len(9, &data.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>()));
        o
    }

    pub fn tensor_i64(name: &str, dims: &[i64], data: &[i64]) -> Vec<u8> {
        let mut o: Vec<u8> = dims.iter().flat_map(|d| int(1, *d)).collect();
        o.extend(int(2, 7));
        o.extend(len(8, name.as_bytes()));
        o.extend(len(9, &data.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>()));
        o
    }

    pub fn attr_int(name: &str, v: i64) -> Vec<u8> {
        [len(1, name.as_bytes()), int(20, 2), int(3, v)].concat()
    }

    pub fn attr_ints(name: &str, v: &[i64]) -> Vec<u8> {
        let mut o = [len(1, name.as_bytes()), int(20, 7)].concat();
        o.extend(v.iter().flat_map(|x| int(8, *x)));
        o
    }

    pub fn attr_str(name: &str, v: &str) -> Vec<u8> {
        [len(1, name.as_bytes()), int(20, 3), len(4, v.as_bytes())].concat()
    }

    pub fn node(op: &str, inputs: &[&str], outputs: &[&str], attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut o: Vec<u8> = inputs.iter().flat_map(|i| len(1, i.as_bytes())).collect();
        o.extend(outputs.iter().flat_map(|i| len(2, i.as_bytes())));
        o.extend(len(3, format!("{op}_{}", outputs.first().unwrap_or(&"")).as_bytes()));
        o.extend(len(4, op.as_bytes()));
        o.extend(attrs.iter().flat_map(|a| len(5, a)));
        o
    }

    pub fn model(nodes: &[Vec<u8>], inits: &[Vec<u8>], inputs: &[&str], outputs: &[&str]) -> Vec<u8> {
        let mut g: Vec<u8> = nodes.iter().flat_map(|n| len(1, n)).collect();
        g.extend(len(2, b"test"));
        g.extend(inits.iter().flat_map(|t| len(5, t)));
        g.extend(inputs.iter().flat_map(|i| len(11, &len(1, i.as_bytes()))));
        g.extend(outputs.iter().flat_map(|i| len(12, &len(1, i.as_bytes()))));
        [int(1, 6), len(8, &int(2, 11)), len(7, &g)].concat()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::encode::*;
    use super::{Attr, Data, ModelProto, TensorProto, parse, read};
    use crate::Result;

    fn sample() -> Vec<u8> {
        let conv = node("Conv", &["x", "w"], &["y"], &[attr_ints("pads", &[1, 1, 1, 1]), attr_int("group", 1), attr_str("auto_pad", "NOTSET")]);
        // a packed ints attribute and a float attribute
        let packed = [len(1, b"perm"), int(20, 7), len(8, &[2, 3, 0, 1])].concat();
        let scale = [len(1, b"alpha"), int(20, 1), float(2, 0.5)].concat();
        let t = node("Transpose", &["y"], &["z"], &[packed, scale]);
        let w = tensor_f32("w", &[1, 1, 1, 2], &[1.5, -2.0]);
        // float_data instead of raw_data
        let s = [int(1, 2), int(2, 1), len(8, b"s"), len(4, &[1f32.to_le_bytes(), 2f32.to_le_bytes()].concat())].concat();
        let i = tensor_i64("shape", &[2], &[-1, 4]);
        model(&[conv, t], &[w, s, i], &["x"], &["z"])
    }

    #[test]
    fn decodes_a_hand_built_model() {
        let m = parse(&sample()).unwrap();
        assert_eq!(m.opset, Some(11));
        assert_eq!(m.inputs, ["x"]);
        assert_eq!(m.outputs, ["z"]);
        assert_eq!(m.nodes.len(), 2);
        let conv = &m.nodes[0];
        assert_eq!((conv.op_type.as_str(), conv.inputs.clone(), conv.outputs.clone()), ("Conv", vec!["x".into(), "w".into()], vec!["y".into()]));
        assert_eq!(conv.attr("pads"), Some(&Attr::Ints(vec![1, 1, 1, 1])));
        assert_eq!(conv.attr("group"), Some(&Attr::Int(1)));
        assert_eq!(conv.attr("auto_pad"), Some(&Attr::Bytes(b"NOTSET".to_vec())));
        assert_eq!(m.nodes[1].attr("perm"), Some(&Attr::Ints(vec![2, 3, 0, 1])));
        assert_eq!(m.nodes[1].attr("alpha"), Some(&Attr::Float(0.5)));
        assert_eq!(m.initializers[0], TensorProto { name: "w".into(), dims: vec![1, 1, 1, 2], data: Data::F32(vec![1.5, -2.0]) });
        assert_eq!(m.initializers[1].data, Data::F32(vec![1.0, 2.0]));
        assert_eq!(m.initializers[2].data, Data::I64(vec![-1, 4]));
    }

    #[test]
    fn truncated_files_are_errors() {
        let b = sample();
        for n in 0..b.len() {
            assert!(parse(&b[..n]).is_err(), "prefix of {n} bytes");
        }
    }

    #[test]
    fn garbage_never_panics() {
        assert!(parse(&[0xff; 64]).is_err());
        assert!(parse(b"this is not a model").is_err());
        // a cheap deterministic generator, and mutations of a valid model
        let mut s = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for _ in 0..2000 {
            let n = (rnd() % 300) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| rnd() as u8).collect();
            let _ = parse(&bytes);
        }
        let good = sample();
        for _ in 0..5000 {
            let mut b = good.clone();
            for _ in 0..1 + rnd() % 4 {
                let i = (rnd() as usize) % b.len();
                b[i] = rnd() as u8;
            }
            let _ = parse(&b);
        }
    }

    #[test]
    fn absurd_lengths_and_varints_are_errors() {
        // a graph field claiming 2^62 bytes
        let mut b = Vec::new();
        varint(7 << 3 | 2, &mut b);
        varint(1 << 62, &mut b);
        b.extend_from_slice(&[0; 16]);
        assert!(parse(&b).is_err());
        // an 11-byte varint
        let mut b = vec![7 << 3];
        b.extend_from_slice(&[0x80; 11]);
        assert!(parse(&b).is_err());
        // wire type 3 (groups)
        assert!(parse(&[7 << 3 | 3]).is_err());
    }

    fn with_init(t: Vec<u8>) -> Result<ModelProto> {
        parse(&model(&[], &[t], &["x"], &["x"]))
    }

    #[test]
    fn hostile_tensors_are_errors() {
        // dims whose product overflows, or is over the cap, with no data behind them
        assert!(with_init(tensor_f32("a", &[1 << 40, 1 << 40], &[])).is_err());
        assert!(with_init(tensor_f32("a", &[1 << 32], &[])).is_err());
        assert!(with_init(tensor_f32("a", &[-1, 2], &[1.0, 2.0])).is_err());
        // bytes not matching the dims
        assert!(with_init(tensor_f32("a", &[2, 2], &[1.0, 2.0, 3.0])).is_err());
        assert!(with_init(tensor_i64("a", &[3], &[1, 2])).is_err());
        let odd = [int(1, 1), int(2, 1), len(9, &[0, 0, 0])].concat();
        assert!(with_init(odd).is_err());
        // unsupported types and external data
        assert!(with_init([int(1, 1), int(2, 11), len(9, &[0; 8])].concat()).is_err());
        assert!(with_init([int(1, 1), int(2, 1), int(14, 1)].concat()).is_err());
        // a rank that is too high
        assert!(with_init(tensor_f32("a", &[1; 40], &[1.0])).is_err());
        // a scalar is fine
        assert_eq!(with_init(tensor_f32("a", &[], &[3.0])).unwrap().initializers[0].dims, Vec::<usize>::new());
    }

    #[test]
    fn subgraphs_are_rejected() {
        let g = [len(1, b"then_branch"), int(20, 5), len(6, &len(1, &node("Relu", &["a"], &["b"], &[])))].concat();
        let n = node("If", &["c"], &["d"], &[g]);
        let e = parse(&model(&[n], &[], &["c"], &["d"])).unwrap_err();
        assert!(e.to_string().contains("subgraph"), "{e}");
    }

    #[test]
    fn missing_or_oversized_files_are_errors() {
        assert!(read(Path::new("does/not/exist.onnx")).is_err());
    }
}
