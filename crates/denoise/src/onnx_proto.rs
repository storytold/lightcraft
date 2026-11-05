//! Bounded, borrowed decoding of the ONNX fields used by denoise, from the public Apache-2.0 schema:
//! https://github.com/onnx/onnx/blob/main/onnx/onnx.proto3
//! No generated code, protoc, recursive graph decoding, or external tensor files.

use crate::net::{MAX_OPS, MAX_WEIGHTS, NetError};

const MAX_ITEMS: usize = MAX_OPS * 4;
const MAX_STRING: usize = 4096;
pub(super) const MAX_BYTES: usize = MAX_WEIGHTS * 4 + 16 * 1024 * 1024;

type Result<T> = std::result::Result<T, NetError>;
fn bad() -> NetError {
    NetError::Invalid("malformed or oversized ONNX protobuf".into())
}

struct Fields<'a> {
    rest: &'a [u8],
    count: usize,
}
enum Wire<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    F32(f32),
    Other,
}

fn varint(rest: &mut &[u8]) -> Result<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let (b, tail) = rest.split_first().ok_or_else(bad)?;
        *rest = tail;
        if shift == 63 && *b > 1 {
            return Err(bad());
        }
        value |= u64::from(*b & 127) << shift;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err(bad())
}

impl<'a> Fields<'a> {
    fn new(rest: &'a [u8]) -> Self {
        Self { rest, count: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let out = self.rest.get(..n).ok_or_else(bad)?;
        self.rest = self.rest.get(n..).ok_or_else(bad)?;
        Ok(out)
    }
    fn next(&mut self) -> Result<Option<(u32, Wire<'a>)>> {
        if self.rest.is_empty() {
            return Ok(None);
        }
        self.count += 1;
        if self.count > MAX_WEIGHTS {
            return Err(bad());
        }
        let tag = varint(&mut self.rest)?;
        let number = u32::try_from(tag >> 3).map_err(|_| bad())?;
        if number == 0 || number >= 1 << 29 {
            return Err(bad());
        }
        let wire = match tag & 7 {
            0 => Wire::Int(varint(&mut self.rest)?),
            1 => {
                self.take(8)?;
                Wire::Other
            }
            2 => {
                let n = usize::try_from(varint(&mut self.rest)?).map_err(|_| bad())?;
                Wire::Bytes(self.take(n)?)
            }
            5 => {
                let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| bad())?;
                Wire::F32(f32::from_le_bytes(b))
            }
            _ => return Err(bad()),
        };
        Ok(Some((number, wire)))
    }
}

fn string(b: &[u8]) -> Result<String> {
    if b.len() > MAX_STRING {
        return Err(bad());
    }
    std::str::from_utf8(b).map(str::to_owned).map_err(|_| bad())
}
fn push<T>(v: &mut Vec<T>, value: T, cap: usize) -> Result<()> {
    if v.len() >= cap {
        return Err(bad());
    }
    v.try_reserve(1).map_err(|_| bad())?;
    v.push(value);
    Ok(())
}
fn ints(w: Wire<'_>, out: &mut Vec<i64>, cap: usize) -> Result<()> {
    match w {
        Wire::Int(v) => push(out, v as i64, cap),
        Wire::Bytes(mut b) => {
            while !b.is_empty() {
                let v = varint(&mut b)?;
                push(out, v as i64, cap)?;
            }
            Ok(())
        }
        _ => Err(bad()),
    }
}

#[derive(Default)]
pub(super) struct AttributeProto {
    pub name: String,
    /// `None` when the file has no float value for the attribute.
    pub f: Option<f32>,
    /// The declared attribute type (1 float, 2 int, 3 string, 7 ints); 0 when the file does not say.
    pub kind: i64,
    pub i: i64,
    pub s: Vec<u8>,
    pub ints: Vec<i64>,
}
#[derive(Default)]
pub(super) struct NodeProto {
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub op_type: String,
    pub attribute: Vec<AttributeProto>,
}
#[derive(Default)]
pub(super) struct TensorProto<'a> {
    pub name: String,
    pub dims: Vec<i64>,
    pub data_type: i32,
    pub raw_data: &'a [u8],
    pub float_data: Vec<f32>,
}
pub(super) struct ValueInfoProto {
    pub name: String,
    pub channels: Option<usize>,
}
#[derive(Default)]
pub(super) struct Graph<'a> {
    pub node: Vec<NodeProto>,
    pub initializer: Vec<TensorProto<'a>>,
    pub input: Vec<ValueInfoProto>,
    pub output: Vec<ValueInfoProto>,
}

fn attribute(b: &[u8]) -> Result<AttributeProto> {
    let mut a = AttributeProto::default();
    let mut fields = Fields::new(b);
    while let Some((n, w)) = fields.next()? {
        match (n, w) {
            (1, Wire::Bytes(b)) => a.name = string(b)?,
            (2, Wire::F32(v)) if a.f.is_none() => a.f = Some(v),
            (3, Wire::Int(v)) => a.i = v as i64,
            (4, Wire::Bytes(b)) => {
                if b.len() > MAX_STRING {
                    return Err(bad());
                }
                a.s = b.to_vec();
            }
            (8, w) => ints(w, &mut a.ints, 16)?,
            (20, Wire::Int(v @ (1 | 2 | 3 | 7))) if a.kind == 0 => a.kind = v as i64,
            _ => return Err(NetError::Unsupported("unknown attribute value".into())),
        }
    }
    Ok(a)
}
fn node(b: &[u8]) -> Result<NodeProto> {
    let mut a = NodeProto::default();
    let mut fields = Fields::new(b);
    while let Some((n, w)) = fields.next()? {
        match (n, w) {
            (1, Wire::Bytes(b)) => push(&mut a.input, string(b)?, 4)?,
            (2, Wire::Bytes(b)) => push(&mut a.output, string(b)?, 2)?,
            (4, Wire::Bytes(b)) => a.op_type = string(b)?,
            (5, Wire::Bytes(b)) => push(&mut a.attribute, attribute(b)?, 16)?,
            (7, Wire::Bytes(b)) if !b.is_empty() => return Err(NetError::Unsupported("a custom operator domain".into())),
            _ => {}
        }
    }
    Ok(a)
}
fn tensor(b: &[u8]) -> Result<TensorProto<'_>> {
    let mut a = TensorProto::default();
    let mut fields = Fields::new(b);
    while let Some((n, w)) = fields.next()? {
        match (n, w) {
            (1, w) => ints(w, &mut a.dims, 8)?,
            (2, Wire::Int(v)) => a.data_type = i32::try_from(v).map_err(|_| bad())?,
            (4, Wire::F32(v)) => push(&mut a.float_data, v, MAX_WEIGHTS)?,
            (4, Wire::Bytes(b)) => {
                if !b.len().is_multiple_of(4) {
                    return Err(bad());
                }
                for chunk in b.as_chunks::<4>().0 {
                    push(&mut a.float_data, f32::from_le_bytes(*chunk), MAX_WEIGHTS)?;
                }
            }
            (8, Wire::Bytes(b)) => a.name = string(b)?,
            (9, Wire::Bytes(b)) => {
                if b.len() > MAX_WEIGHTS * 4 {
                    return Err(bad());
                }
                a.raw_data = b;
            }
            (13, _) | (14, Wire::Int(1..)) => return Err(NetError::Unsupported("external tensor data".into())),
            _ => {}
        }
    }
    Ok(a)
}
fn channel_shape(b: &[u8]) -> Result<Option<usize>> {
    let mut fields = Fields::new(b);
    let mut dim = 0;
    let mut channels = None;
    while let Some((n, w)) = fields.next()? {
        if let (1, Wire::Bytes(b)) = (n, w) {
            if dim >= 8 {
                return Err(bad());
            }
            let mut df = Fields::new(b);
            while let Some((n, w)) = df.next()? {
                if let (1, Wire::Int(v)) = (n, w)
                    && dim == 1
                {
                    channels = usize::try_from(v).ok().filter(|&v| v > 0);
                }
            }
            dim += 1;
        }
    }
    Ok(channels)
}
fn channel_type(b: &[u8]) -> Result<Option<usize>> {
    let mut fields = Fields::new(b);
    while let Some((n, w)) = fields.next()? {
        if let (1, Wire::Bytes(b)) = (n, w) {
            let mut tf = Fields::new(b);
            let (mut elem, mut channels) = (0, None);
            while let Some((n, w)) = tf.next()? {
                match (n, w) {
                    (1, Wire::Int(v)) => elem = v,
                    (2, Wire::Bytes(b)) => channels = channel_shape(b)?,
                    _ => {}
                }
            }
            return Ok(if elem == 1 { channels } else { None });
        }
    }
    Ok(None)
}
fn value_info(b: &[u8]) -> Result<ValueInfoProto> {
    let mut out = ValueInfoProto { name: String::new(), channels: None };
    let mut fields = Fields::new(b);
    while let Some((n, w)) = fields.next()? {
        match (n, w) {
            (1, Wire::Bytes(b)) => out.name = string(b)?,
            (2, Wire::Bytes(b)) => out.channels = channel_type(b)?,
            _ => {}
        }
    }
    Ok(out)
}
fn graph(b: &[u8]) -> Result<Graph<'_>> {
    let mut out = Graph::default();
    let mut fields = Fields::new(b);
    while let Some((n, w)) = fields.next()? {
        match (n, w) {
            (1, Wire::Bytes(b)) => push(&mut out.node, node(b)?, MAX_ITEMS)?,
            (5, Wire::Bytes(b)) => push(&mut out.initializer, tensor(b)?, MAX_ITEMS)?,
            (11, Wire::Bytes(b)) => push(&mut out.input, value_info(b)?, MAX_ITEMS)?,
            (12, Wire::Bytes(b)) => push(&mut out.output, value_info(b)?, 1)?,
            (15, _) => return Err(NetError::Unsupported("sparse initializers".into())),
            _ => {}
        }
    }
    Ok(out)
}
pub(super) fn decode(b: &[u8]) -> Result<Graph<'_>> {
    if b.len() > MAX_BYTES {
        return Err(bad());
    }
    let mut fields = Fields::new(b);
    let mut out = None;
    while let Some((n, w)) = fields.next()? {
        if let (7, Wire::Bytes(b)) = (n, w) {
            if out.is_some() {
                return Err(bad());
            }
            out = Some(graph(b)?);
        }
    }
    out.ok_or_else(bad)
}
