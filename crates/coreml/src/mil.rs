//! Writing a Core ML *ML Program* (`.mlpackage`) from Rust, for networks the older `NeuralNetwork` format does not
//! run well: transformers (layer norm, attention, GELU), which the Neural Engine runs in 16-bit arithmetic.
//!
//! A package is a folder: `Manifest.json`, `Data/com.apple.CoreML/model.mlmodel` (a protobuf `Model` holding an
//! `mlProgram`: one function whose block lists operations, each naming its inputs and giving its output a type) and
//! `Data/com.apple.CoreML/weights/weight.bin` (the big constants, 16-bit, each behind a 64-byte header). The field
//! numbers and the blob layout were read from coremltools' protobuf descriptors and its output for the same network.
//!
//! [`Builder`] writes the weights as they are given (one tensor in memory at a time) and keeps only the operations,
//! so a model of hundreds of millions of weights is made without holding them. Shapes are tracked here: every output's
//! type is written into the model, so an operation whose inputs do not fit is an [`Error::Build`], not a model Core ML
//! rejects. Nothing here is `unsafe` and nothing panics: every size is checked.
//!
//! Precision: constants and arithmetic are 16-bit (the input and the result are 32-bit, cast at the ends), which is
//! what the Neural Engine computes in. The caller keeps its values inside 16-bit range.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::Error;

/// Elements allowed in one tensor (a hostile shape must not make a huge allocation or overflow).
const MAX_ELEMENTS: usize = 1 << 31;
/// Rank allowed.
const MAX_RANK: usize = 8;

const WEIGHTS_FILE: &str = "@model_path/weights/weight.bin";
/// Blobs and the first blob start at multiples of this.
const BLOB_ALIGN: u64 = 64;
const BLOB_MAGIC: u32 = 0xdead_beef;
const BLOB_FLOAT16: u32 = 1;

/// The model's operation set (macOS 14 / iOS 17).
const OPSET: &str = "CoreML7";
const SPEC_VERSION: u64 = 8;

/// Element types of a tensor in the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dtype {
    F16,
    F32,
    I32,
    Bool,
    Str,
}

impl Dtype {
    /// The `DataType` number in the protobuf.
    fn code(self) -> u64 {
        match self {
            Dtype::Bool => 1,
            Dtype::Str => 2,
            Dtype::F16 => 10,
            Dtype::F32 => 11,
            Dtype::I32 => 23,
        }
    }
}

/// A value of the program: its name there, its element type and its shape (empty for a scalar).
#[derive(Clone, Debug, PartialEq)]
pub struct Val {
    name: String,
    dtype: Dtype,
    shape: Vec<usize>,
}

impl Val {
    /// The value's name in the program (what the loaded model calls an output).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tensor's shape.
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
}

fn bad<T>(why: impl Into<String>) -> Result<T, Error> {
    Err(Error::Build(why.into()))
}

/// The number of elements of a shape, `None` past [`MAX_ELEMENTS`] or past [`MAX_RANK`] dimensions.
fn elements(shape: &[usize]) -> Option<usize> {
    if shape.len() > MAX_RANK {
        return None;
    }
    shape.iter().try_fold(1usize, |a, d| a.checked_mul(*d)).filter(|n| *n <= MAX_ELEMENTS)
}

// ---------------------------------------------------------------------------------------------------------------------
// 16-bit numbers

/// `v` as the nearest 16-bit float (ties to even); too large becomes infinity, which the caller avoids.
pub fn to_f16(v: f32) -> u16 {
    let bits = v.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exp = ((bits >> 23) & 0xff) as i32;
    let man = bits & 0x007f_ffff;
    if exp == 0xff {
        // infinity or NaN (a NaN keeps a non-zero fraction)
        return sign | 0x7c00 | if man != 0 { 0x0200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        // subnormal or zero
        if e < -10 {
            return sign;
        }
        let m = man | 0x0080_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let rest = m & ((1 << shift) - 1);
        let mid = 1 << (shift - 1);
        let up = rest > mid || (rest == mid && (half & 1) == 1);
        return sign | (half + u32::from(up)) as u16;
    }
    let half = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1fff;
    let up = rest > 0x1000 || (rest == 0x1000 && (half & 1) == 1);
    // a carry out of the fraction raises the exponent, which is right (and reaches infinity at the top)
    sign | (half + u32::from(up)) as u16
}

// ---------------------------------------------------------------------------------------------------------------------
// protobuf

mod pb {
    pub fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn tag(field: u32, wire: u8, out: &mut Vec<u8>) {
        varint((u64::from(field) << 3) | u64::from(wire), out);
    }

    pub fn uint(field: u32, v: u64, out: &mut Vec<u8>) {
        tag(field, 0, out);
        varint(v, out);
    }

    pub fn bytes(field: u32, b: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }

    pub fn string(field: u32, s: &str, out: &mut Vec<u8>) {
        bytes(field, s.as_bytes(), out);
    }

    /// A packed repeated varint field (an empty list writes nothing).
    pub fn packed(field: u32, values: impl IntoIterator<Item = u64>, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        for v in values {
            varint(v, &mut body);
        }
        if !body.is_empty() {
            bytes(field, &body, out);
        }
    }

    /// A message field built by `f`.
    pub fn message(field: u32, out: &mut Vec<u8>, f: impl FnOnce(&mut Vec<u8>)) {
        let mut body = Vec::new();
        f(&mut body);
        bytes(field, &body, out);
    }

    /// One entry of a protobuf map with a string key and a message value built by `f`.
    pub fn entry(field: u32, key: &str, out: &mut Vec<u8>, f: impl FnOnce(&mut Vec<u8>)) {
        message(field, out, |e| {
            string(1, key, e);
            message(2, e, f);
        });
    }
}

/// `ValueType { tensorType { dataType, rank, dimensions } }`.
fn value_type(dtype: Dtype, shape: &[usize], out: &mut Vec<u8>) {
    pb::message(1, out, |t| {
        pb::uint(1, dtype.code(), t);
        pb::uint(2, shape.len() as u64, t);
        for d in shape {
            pb::message(3, t, |dim| pb::message(1, dim, |c| pb::uint(1, *d as u64, c)));
        }
    });
}

/// `NamedValueType { name, type }` as the field `field` of `out`.
fn named_type(field: u32, name: &str, dtype: Dtype, shape: &[usize], out: &mut Vec<u8>) {
    pb::message(field, out, |n| {
        pb::string(1, name, n);
        pb::message(2, n, |t| value_type(dtype, shape, t));
    });
}

/// `Value { type, immediateValue { tensor { <kind> { values } } } }` for a constant given inline.
fn immediate(dtype: Dtype, shape: &[usize], kind: u32, values: &[u8], out: &mut Vec<u8>) {
    pb::message(2, out, |t| value_type(dtype, shape, t));
    pb::message(3, out, |imm| pb::message(1, imm, |tensor| pb::message(kind, tensor, |r| r.extend_from_slice(values))));
}

/// The `name` attribute's value: the string `name`.
fn name_value(name: &str, out: &mut Vec<u8>) {
    let mut s = Vec::new();
    pb::string(1, name, &mut s);
    immediate(Dtype::Str, &[], 4, &s, out);
}

/// What the program holds in the folder.
pub struct Builder {
    dir: PathBuf,
    weights: BufWriter<File>,
    /// Where the next blob starts in the weights file.
    at: u64,
    blobs: u32,
    ops: Vec<Vec<u8>>,
    counter: usize,
    input: Option<(String, Vec<usize>)>,
}

impl Builder {
    /// Starts a package in `dir` (made if missing; the files of an earlier package there are replaced).
    pub fn create(dir: &Path) -> Result<Builder, Error> {
        let data = dir.join("Data").join("com.apple.CoreML");
        let io = |e: std::io::Error| Error::Build(format!("{}: {e}", dir.display()));
        std::fs::create_dir_all(data.join("weights")).map_err(io)?;
        let file = File::create(data.join("weights").join("weight.bin")).map_err(io)?;
        let mut weights = BufWriter::new(file);
        // the file's header: the number of blobs (written at the end) and the format's version
        weights.write_all(&[0u8; BLOB_ALIGN as usize]).map_err(io)?;
        Ok(Builder { dir: dir.to_path_buf(), weights, at: BLOB_ALIGN, blobs: 0, ops: Vec::new(), counter: 0, input: None })
    }

    fn name(&mut self, op: &str) -> String {
        self.counter += 1;
        format!("{op}_{}", self.counter)
    }

    /// An operation: its type, named inputs (`key`, value), and its output; returns the output.
    fn op(&mut self, kind: &str, inputs: &[(&str, &Val)], dtype: Dtype, shape: Vec<usize>) -> Result<Val, Error> {
        let lists: Vec<(&str, Vec<&str>)> = inputs.iter().map(|(k, v)| (*k, vec![v.name.as_str()])).collect();
        self.op_of(kind, &lists, dtype, shape)
    }

    /// [`Builder::op`] where an input may be a list of values (`concat`'s `values`).
    fn op_of(&mut self, kind: &str, inputs: &[(&str, Vec<&str>)], dtype: Dtype, shape: Vec<usize>) -> Result<Val, Error> {
        if elements(&shape).is_none() {
            return bad(format!("{kind}: a tensor of shape {shape:?} is too large"));
        }
        let out = Val { name: self.name(kind), dtype, shape };
        let mut op = Vec::new();
        pb::string(1, kind, &mut op);
        for (key, names) in inputs {
            pb::entry(2, key, &mut op, |arg| {
                for n in names {
                    pb::message(1, arg, |binding| pb::string(1, n, binding));
                }
            });
        }
        named_type(3, &out.name, out.dtype, &out.shape, &mut op);
        pb::entry(5, "name", &mut op, |v| name_value(&out.name, v));
        self.ops.push(op);
        Ok(out)
    }

    /// A `const` operation whose value is built by `value` (the body of its `val` attribute).
    fn constant(&mut self, dtype: Dtype, shape: &[usize], value: impl FnOnce(&mut Vec<u8>)) -> Val {
        self.counter += 1;
        let name = format!("const_{}", self.counter);
        let mut op = Vec::new();
        pb::string(1, "const", &mut op);
        named_type(3, &name, dtype, shape, &mut op);
        pb::entry(5, "val", &mut op, value);
        pb::entry(5, "name", &mut op, |v| name_value(&name, v));
        self.ops.push(op);
        Val { name, dtype, shape: shape.to_vec() }
    }

    /// The model's one input (32-bit, `shape`); call it once.
    pub fn input(&mut self, name: &str, shape: &[usize]) -> Result<Val, Error> {
        if self.input.is_some() {
            return bad("the model has one input");
        }
        if elements(shape).is_none() || shape.is_empty() {
            return bad(format!("the input's shape {shape:?} is not usable"));
        }
        self.input = Some((name.to_string(), shape.to_vec()));
        Ok(Val { name: name.to_string(), dtype: Dtype::F32, shape: shape.to_vec() })
    }

    /// A constant tensor of 16-bit weights, written to the weights file: `data` has `elements(shape)` numbers.
    pub fn weight(&mut self, shape: &[usize], data: &[f32]) -> Result<Val, Error> {
        if elements(shape) != Some(data.len()) {
            return bad(format!("a weight of shape {shape:?} was given {} numbers", data.len()));
        }
        let size = (data.len() as u64).checked_mul(2).ok_or_else(|| Error::Build("a weight is too large".into()))?;
        let io = |e: std::io::Error| Error::Build(format!("the weights: {e}"));
        let start = self.at;
        let mut header = [0u8; BLOB_ALIGN as usize];
        header[0..4].copy_from_slice(&BLOB_MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&BLOB_FLOAT16.to_le_bytes());
        header[8..16].copy_from_slice(&size.to_le_bytes());
        header[16..24].copy_from_slice(&(start + BLOB_ALIGN).to_le_bytes());
        self.weights.write_all(&header).map_err(io)?;
        let mut chunk = Vec::with_capacity(1 << 16);
        for part in data.chunks(1 << 15) {
            chunk.clear();
            chunk.extend(part.iter().flat_map(|v| to_f16(*v).to_le_bytes()));
            self.weights.write_all(&chunk).map_err(io)?;
        }
        let padded = size.div_ceil(BLOB_ALIGN) * BLOB_ALIGN;
        let pad = vec![0u8; (padded - size) as usize];
        self.weights.write_all(&pad).map_err(io)?;
        self.at = start + BLOB_ALIGN + padded;
        self.blobs = self.blobs.checked_add(1).ok_or_else(|| Error::Build("too many weights".into()))?;
        Ok(self.constant(Dtype::F16, shape, |v| {
            pb::message(2, v, |t| value_type(Dtype::F16, shape, t));
            pb::message(5, v, |blob| {
                pb::string(1, WEIGHTS_FILE, blob);
                pb::uint(2, start, blob);
            });
        }))
    }

    /// A 16-bit scalar.
    pub fn scalar(&mut self, v: f32) -> Val {
        self.constant(Dtype::F16, &[], |val| {
            let mut b = Vec::new();
            pb::bytes(1, &to_f16(v).to_le_bytes(), &mut b);
            immediate(Dtype::F16, &[], 7, &b, val);
        })
    }

    /// An `int32` vector.
    fn ints(&mut self, values: &[usize]) -> Result<Val, Error> {
        let mut vals = Vec::with_capacity(values.len());
        for v in values {
            vals.push(i32::try_from(*v).map_err(|_| Error::Build("a dimension is too large".into()))?);
        }
        Ok(self.constant(Dtype::I32, &[vals.len()], |val| {
            let mut ints = Vec::new();
            pb::packed(1, vals.iter().map(|v| *v as u64), &mut ints);
            immediate(Dtype::I32, &[vals.len()], 2, &ints, val);
        }))
    }

    /// An `int32` scalar.
    fn int(&mut self, v: usize) -> Result<Val, Error> {
        let v = i32::try_from(v).map_err(|_| Error::Build("a number is too large".into()))?;
        Ok(self.constant(Dtype::I32, &[], |val| {
            let mut ints = Vec::new();
            pb::packed(1, [v as u64], &mut ints);
            immediate(Dtype::I32, &[], 2, &ints, val);
        }))
    }

    fn boolean(&mut self, v: bool) -> Val {
        self.constant(Dtype::Bool, &[], |val| {
            let mut b = Vec::new();
            pb::packed(1, [u64::from(v)], &mut b);
            immediate(Dtype::Bool, &[], 3, &b, val);
        })
    }

    fn string(&mut self, s: &str) -> Val {
        self.constant(Dtype::Str, &[], |val| {
            let mut b = Vec::new();
            pb::string(1, s, &mut b);
            immediate(Dtype::Str, &[], 4, &b, val);
        })
    }

    /// `x` as 16-bit floats.
    pub fn to_f16(&mut self, x: &Val) -> Result<Val, Error> {
        let dtype = self.string("fp16");
        self.op("cast", &[("x", x), ("dtype", &dtype)], Dtype::F16, x.shape.clone())
    }

    /// `x` as 32-bit floats.
    pub fn to_f32(&mut self, x: &Val) -> Result<Val, Error> {
        let dtype = self.string("fp32");
        self.op("cast", &[("x", x), ("dtype", &dtype)], Dtype::F32, x.shape.clone())
    }

    /// Layer normalisation over the last axis, with scale `gamma` and shift `beta` (vectors of that size).
    pub fn layer_norm(&mut self, x: &Val, gamma: &Val, beta: &Val, epsilon: f32) -> Result<Val, Error> {
        let last = x.shape.last().copied().ok_or_else(|| Error::Build("layer_norm of a scalar".into()))?;
        if gamma.shape != [last] || beta.shape != [last] {
            return bad(format!("layer_norm over {last}: gamma {:?} and beta {:?} do not fit", gamma.shape, beta.shape));
        }
        let axes = self.ints(&[x.shape.len() - 1])?;
        let eps = self.scalar(epsilon);
        self.op("layer_norm", &[("x", x), ("axes", &axes), ("gamma", gamma), ("beta", beta), ("epsilon", &eps)], x.dtype, x.shape.clone())
    }

    /// `x · weightᵀ + bias` over the last axis: `weight` is `[out, in]`, `bias` `[out]`.
    pub fn linear(&mut self, x: &Val, weight: &Val, bias: &Val) -> Result<Val, Error> {
        let (Some(last), [out, inn]) = (x.shape.last().copied(), weight.shape.as_slice()) else {
            return bad(format!("linear of {:?} by {:?}", x.shape, weight.shape));
        };
        if last != *inn || bias.shape != [*out] {
            return bad(format!("linear: input {:?}, weight {:?}, bias {:?} do not fit", x.shape, weight.shape, bias.shape));
        }
        let mut shape = x.shape.clone();
        if let Some(l) = shape.last_mut() {
            *l = *out;
        }
        self.op("linear", &[("x", x), ("weight", weight), ("bias", bias)], x.dtype, shape)
    }

    /// `x` with another shape of as many elements.
    pub fn reshape(&mut self, x: &Val, shape: &[usize]) -> Result<Val, Error> {
        if elements(shape).is_none() || elements(&x.shape) != elements(shape) {
            return bad(format!("reshape {:?} to {shape:?}", x.shape));
        }
        let s = self.ints(shape)?;
        self.op("reshape", &[("x", x), ("shape", &s)], x.dtype, shape.to_vec())
    }

    /// `x` with its axes in the order `perm`.
    pub fn transpose(&mut self, x: &Val, perm: &[usize]) -> Result<Val, Error> {
        let mut seen = vec![false; x.shape.len()];
        let ok = perm.len() == x.shape.len()
            && perm.iter().all(|p| match seen.get_mut(*p) {
                Some(s) if !*s => {
                    *s = true;
                    true
                }
                _ => false,
            });
        if !ok {
            return bad(format!("transpose of {:?} by {perm:?}", x.shape));
        }
        let shape: Vec<usize> = perm.iter().filter_map(|p| x.shape.get(*p).copied()).collect();
        let p = self.ints(perm)?;
        self.op("transpose", &[("x", x), ("perm", &p)], x.dtype, shape)
    }

    /// The shape elementwise operations on `a` and `b` give (numpy broadcasting).
    fn broadcast(a: &[usize], b: &[usize]) -> Result<Vec<usize>, Error> {
        let n = a.len().max(b.len());
        let mut out = vec![1; n];
        for i in 0..n {
            let da = if i < n - a.len() { 1 } else { a.get(i - (n - a.len())).copied().unwrap_or(1) };
            let db = if i < n - b.len() { 1 } else { b.get(i - (n - b.len())).copied().unwrap_or(1) };
            let d = match (da, db) {
                (x, y) if x == y => x,
                (1, y) => y,
                (x, 1) => x,
                _ => return bad(format!("shapes {a:?} and {b:?} do not broadcast")),
            };
            if let Some(o) = out.get_mut(i) {
                *o = d;
            }
        }
        Ok(out)
    }

    /// `x * y`, broadcasting.
    pub fn mul(&mut self, x: &Val, y: &Val) -> Result<Val, Error> {
        let shape = Self::broadcast(&x.shape, &y.shape)?;
        self.op("mul", &[("x", x), ("y", y)], x.dtype, shape)
    }

    /// `x + y`, broadcasting.
    pub fn add(&mut self, x: &Val, y: &Val) -> Result<Val, Error> {
        let shape = Self::broadcast(&x.shape, &y.shape)?;
        self.op("add", &[("x", x), ("y", y)], x.dtype, shape)
    }

    /// The part of `x` from `begin` up to `end` on every axis.
    pub fn slice(&mut self, x: &Val, begin: &[usize], end: &[usize]) -> Result<Val, Error> {
        let n = x.shape.len();
        let fits = begin.len() == n
            && end.len() == n
            && (0..n).all(|i| matches!((begin.get(i), end.get(i), x.shape.get(i)), (Some(b), Some(e), Some(d)) if b < e && e <= d));
        if !fits {
            return bad(format!("slice {begin:?}..{end:?} of {:?}", x.shape));
        }
        let shape: Vec<usize> = begin.iter().zip(end).map(|(b, e)| e - b).collect();
        let (b, e) = (self.ints(begin)?, self.ints(end)?);
        let (bm, em) = (self.bools(n, false), self.bools(n, false));
        self.op("slice_by_index", &[("x", x), ("begin", &b), ("end", &e), ("begin_mask", &bm), ("end_mask", &em)], x.dtype, shape)
    }

    /// The values joined along `axis`, which they must agree on everywhere else.
    pub fn concat(&mut self, xs: &[&Val], axis: usize) -> Result<Val, Error> {
        let Some(first) = xs.first() else {
            return bad("concat of nothing");
        };
        let mut shape = first.shape.clone();
        let mut total = 0usize;
        for x in xs {
            let same = x.shape.len() == shape.len()
                && x.dtype == first.dtype
                && x.shape.iter().enumerate().all(|(i, d)| i == axis || shape.get(i) == Some(d));
            let Some(d) = x.shape.get(axis).filter(|_| same) else {
                return bad(format!("concat of {:?} and {:?} along {axis}", first.shape, x.shape));
            };
            total = total.checked_add(*d).ok_or_else(|| Error::Build("concat is too large".into()))?;
        }
        if let Some(a) = shape.get_mut(axis) {
            *a = total;
        }
        let (a, interleave) = (self.int(axis)?, self.boolean(false));
        let names: Vec<&str> = xs.iter().map(|x| x.name.as_str()).collect();
        self.op_of(
            "concat",
            &[("values", names), ("axis", vec![a.name.as_str()]), ("interleave", vec![interleave.name.as_str()])],
            first.dtype,
            shape,
        )
    }

    /// A vector of `n` booleans, all `v`.
    fn bools(&mut self, n: usize, v: bool) -> Val {
        self.constant(Dtype::Bool, &[n], |val| {
            let mut b = Vec::new();
            pb::packed(1, std::iter::repeat_n(u64::from(v), n), &mut b);
            immediate(Dtype::Bool, &[n], 3, &b, val);
        })
    }

    /// `x · y` over the last two axes (the leading axes are equal, or `y` has none: it is shared), with `y` transposed
    /// first when `transpose_y`.
    pub fn matmul(&mut self, x: &Val, y: &Val, transpose_y: bool) -> Result<Val, Error> {
        let (rx, ry) = (x.shape.len(), y.shape.len());
        if rx < 2 || ry < 2 || ry > rx {
            return bad(format!("matmul of {:?} by {:?}", x.shape, y.shape));
        }
        let k = x.shape.get(rx - 1).copied().unwrap_or(0);
        let (a, b) = (y.shape.get(ry - 2).copied().unwrap_or(0), y.shape.get(ry - 1).copied().unwrap_or(0));
        let (ky, n) = if transpose_y { (b, a) } else { (a, b) };
        let shared = ry == 2 || x.shape.get(rx - ry..rx - 2) == y.shape.get(..ry - 2);
        if ky != k || !shared {
            return bad(format!("matmul: {:?} by {:?}{}", x.shape, y.shape, if transpose_y { " transposed" } else { "" }));
        }
        let mut shape = x.shape.clone();
        if let Some(s) = shape.get_mut(rx - 1) {
            *s = n;
        }
        let (tx, ty) = (self.boolean(false), self.boolean(transpose_y));
        self.op("matmul", &[("x", x), ("y", y), ("transpose_x", &tx), ("transpose_y", &ty)], x.dtype, shape)
    }

    /// Softmax over the last axis.
    pub fn softmax(&mut self, x: &Val) -> Result<Val, Error> {
        let axis = self.int(x.shape.len().saturating_sub(1))?;
        self.op("softmax", &[("x", x), ("axis", &axis)], x.dtype, x.shape.clone())
    }

    /// GELU (the exact one, with the error function).
    pub fn gelu(&mut self, x: &Val) -> Result<Val, Error> {
        let mode = self.string("EXACT");
        self.op("gelu", &[("x", x), ("mode", &mode)], x.dtype, x.shape.clone())
    }

    /// Finishes the package with `output` (32-bit) as the model's result; returns the package folder.
    pub fn finish(mut self, output: &Val) -> Result<PathBuf, Error> {
        let Some((input_name, input_shape)) = self.input.clone() else {
            return bad("the model has no input");
        };
        if output.dtype != Dtype::F32 {
            return bad("the model's output must be 32-bit: cast it with to_f32");
        }
        let io = |e: std::io::Error| Error::Build(format!("{}: {e}", self.dir.display()));
        // the weights file's header: the number of blobs and the format's version (2)
        self.weights.flush().map_err(io)?;
        let mut file = self.weights.into_inner().map_err(|e| Error::Build(format!("the weights: {e}")))?;
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        let mut head = [0u8; 8];
        head[0..4].copy_from_slice(&self.blobs.to_le_bytes());
        head[4..8].copy_from_slice(&2u32.to_le_bytes());
        file.write_all(&head).map_err(io)?;
        file.sync_all().map_err(io)?;
        drop(file);

        let mut block = Vec::new();
        pb::string(2, &output.name, &mut block);
        for op in &self.ops {
            pb::bytes(3, op, &mut block);
        }
        let mut function = Vec::new();
        named_type(1, &input_name, Dtype::F32, &input_shape, &mut function);
        pb::string(2, OPSET, &mut function);
        pb::entry(3, OPSET, &mut function, |b| b.extend_from_slice(&block));
        let mut program = Vec::new();
        pb::uint(1, 1, &mut program);
        pb::entry(2, "main", &mut program, |f| f.extend_from_slice(&function));

        let feature = |field: u32, name: &str, shape: &[usize], out: &mut Vec<u8>| {
            pb::message(field, out, |f| {
                pb::string(1, name, f);
                pb::message(3, f, |t| {
                    pb::message(5, t, |m| {
                        pb::packed(1, shape.iter().map(|d| *d as u64), m);
                        pb::uint(2, 65568, m); // FLOAT32
                    });
                });
            });
        };
        let mut model = Vec::new();
        pb::uint(1, SPEC_VERSION, &mut model);
        pb::message(2, &mut model, |d| {
            feature(1, &input_name, &input_shape, d);
            feature(10, &output.name, &output.shape, d);
        });
        pb::bytes(502, &program, &mut model);

        let data = self.dir.join("Data").join("com.apple.CoreML");
        std::fs::write(data.join("model.mlmodel"), &model).map_err(io)?;
        std::fs::write(self.dir.join("Manifest.json"), MANIFEST).map_err(io)?;
        Ok(self.dir)
    }
}

/// The package's table of contents (the identifiers only have to agree with each other).
const MANIFEST: &str = r#"{
    "fileFormatVersion": "1.0.0",
    "itemInfoEntries": {
        "2397A7D4-B0FB-4A8E-B366-7F10FD5C4A49": {
            "author": "com.apple.CoreML",
            "description": "CoreML Model Weights",
            "name": "weights",
            "path": "com.apple.CoreML/weights"
        },
        "CBEB60A5-1895-420C-8020-A4B633846DCB": {
            "author": "com.apple.CoreML",
            "description": "CoreML Model Specification",
            "name": "model.mlmodel",
            "path": "com.apple.CoreML/model.mlmodel"
        }
    },
    "rootModelIdentifier": "CBEB60A5-1895-420C-8020-A4B633846DCB"
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::f16_to_f32;

    #[test]
    fn every_half_float_survives_a_round_trip() {
        for h in 0..=u16::MAX {
            let v = f16_to_f32(h);
            if v.is_nan() {
                assert!(f16_to_f32(to_f16(v)).is_nan());
            } else {
                assert_eq!(to_f16(v), h, "{h:#06x} = {v}");
            }
        }
    }

    #[test]
    fn rounding_goes_to_the_nearest_even_and_saturates() {
        assert_eq!(to_f16(1.0), 0x3c00);
        assert_eq!(to_f16(1.0 + 1.0 / 2048.0), 0x3c00, "a tie goes to the even neighbour");
        assert_eq!(to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
        assert_eq!(to_f16(65504.0), 0x7bff);
        assert_eq!(to_f16(1e9), 0x7c00);
        assert_eq!(to_f16(-1e9), 0xfc00);
        assert_eq!(to_f16(1e-9), 0);
        assert_eq!(to_f16(-0.0), 0x8000);
        assert_eq!(to_f16(5.960_464_5e-8), 1, "the smallest subnormal");
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-mil-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn shapes_that_do_not_fit_are_errors() {
        let d = dir("shapes");
        let mut b = Builder::create(&d).unwrap();
        let x = b.input("x", &[2, 3, 4]).unwrap();
        assert!(b.input("y", &[1]).is_err(), "one input");
        assert!(b.reshape(&x, &[5, 5]).is_err());
        assert!(b.transpose(&x, &[0, 1]).is_err());
        assert!(b.transpose(&x, &[0, 1, 1]).is_err());
        let w = b.weight(&[5, 4], &[0.0; 20]).unwrap();
        let bias = b.weight(&[4], &[0.0; 4]).unwrap();
        assert!(b.linear(&x, &w, &bias).is_err(), "bias is the wrong size");
        assert!(b.weight(&[2, 2], &[0.0; 3]).is_err());
        assert!(b.slice(&x, &[0, 0, 0], &[2, 3, 5]).is_err());
        assert!(b.matmul(&x, &x, false).is_err());
        assert!(b.reshape(&x, &[usize::MAX, 2]).is_err());
        let half = b.to_f16(&x).unwrap();
        assert!(b.finish(&half).is_err(), "the output must be 32-bit");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_weights_file_has_aligned_blobs() {
        let d = dir("blobs");
        let mut b = Builder::create(&d).unwrap();
        let x = b.input("x", &[1, 3]).unwrap();
        let w = b.weight(&[3], &[1.0, 2.0, 3.0]).unwrap();
        let w2 = b.weight(&[2], &[4.0, 5.0]).unwrap();
        let h = b.to_f16(&x).unwrap();
        let y = b.add(&h, &w).unwrap();
        let y = b.to_f32(&y).unwrap();
        let _ = w2;
        let out = b.finish(&y).unwrap();
        let bytes = std::fs::read(out.join("Data/com.apple.CoreML/weights/weight.bin")).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 2, "two blobs");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2, "version");
        assert_eq!(u32::from_le_bytes(bytes[64..68].try_into().unwrap()), BLOB_MAGIC);
        assert_eq!(u64::from_le_bytes(bytes[72..80].try_into().unwrap()), 6, "three halves");
        assert_eq!(u64::from_le_bytes(bytes[80..88].try_into().unwrap()), 128, "its data starts after the header");
        assert_eq!(bytes[128..134], [0x00, 0x3c, 0x00, 0x40, 0x00, 0x42], "1, 2, 3 as halves");
        assert_eq!(u32::from_le_bytes(bytes[192..196].try_into().unwrap()), BLOB_MAGIC, "the next blob is 64-aligned");
        assert!(out.join("Manifest.json").exists() && out.join("Data/com.apple.CoreML/model.mlmodel").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A small transformer-like block, run by Core ML on the processor and compared with the same arithmetic here.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_program_runs_in_core_ml_and_matches_the_same_arithmetic() {
        use crate::{ComputeUnits, Model, compile};
        let d = dir("run");
        let (n, c, heads, hd) = (6usize, 8usize, 2usize, 4usize);
        let mut state = 12345u32;
        let mut rnd = |scale: f32| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / (1 << 24) as f32 - 0.5) * scale
        };
        let wq: Vec<f32> = (0..c * c).map(|_| rnd(0.5)).collect();
        let bq: Vec<f32> = (0..c).map(|_| rnd(0.2)).collect();
        let gamma: Vec<f32> = (0..c).map(|_| 1.0 + rnd(0.2)).collect();
        let beta: Vec<f32> = (0..c).map(|_| rnd(0.2)).collect();
        let input: Vec<f32> = (0..n * c).map(|_| rnd(4.0)).collect();

        let mut b = Builder::create(&d).unwrap();
        let x = b.input("x", &[1, n, c]).unwrap();
        let h = b.to_f16(&x).unwrap();
        let (g, be) = (b.weight(&[c], &gamma).unwrap(), b.weight(&[c], &beta).unwrap());
        let y = b.layer_norm(&h, &g, &be, 1e-5).unwrap();
        let (w, bi) = (b.weight(&[c, c], &wq).unwrap(), b.weight(&[c], &bq).unwrap());
        let q = b.linear(&y, &w, &bi).unwrap();
        let q = b.reshape(&q, &[1, n, heads, hd]).unwrap();
        let q = b.transpose(&q, &[0, 2, 1, 3]).unwrap();
        let s = b.matmul(&q, &q, true).unwrap();
        let scale = b.scalar(0.5);
        let s = b.mul(&s, &scale).unwrap();
        let p = b.softmax(&s).unwrap();
        let o = b.matmul(&p, &q, false).unwrap();
        let o = b.transpose(&o, &[0, 2, 1, 3]).unwrap();
        let o = b.reshape(&o, &[1, n, c]).unwrap();
        let o = b.gelu(&o).unwrap();
        let o = b.add(&h, &o).unwrap();
        let first = b.slice(&o, &[0, 0, 0], &[1, n, c / 2]).unwrap();
        let rest = b.slice(&o, &[0, 0, c / 2], &[1, n, c]).unwrap();
        let o = b.concat(&[&rest, &first], 2).unwrap();
        let o = b.to_f32(&o).unwrap();
        let package = b.finish(&o).unwrap();

        let compiled = compile(&package, &d).unwrap();
        let model = Model::load(&compiled, ComputeUnits::CpuOnly, "x", &o.name).unwrap();
        let got = model.predict(&[1, n, c], &input).unwrap();
        assert_eq!(got.shape, [1, n, c]);

        // the same arithmetic in f32
        let mut expect = vec![0f32; n * c];
        let mut hs = vec![0f32; n * c];
        let mut q = vec![0f32; n * c];
        for t in 0..n {
            let row = &input[t * c..(t + 1) * c];
            let mean = row.iter().sum::<f32>() / c as f32;
            let var = row.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / c as f32;
            for i in 0..c {
                hs[t * c + i] = input[t * c + i];
            }
            let yrow: Vec<f32> = (0..c).map(|i| (row[i] - mean) / (var + 1e-5).sqrt() * gamma[i] + beta[i]).collect();
            for o in 0..c {
                q[t * c + o] = bq[o] + (0..c).map(|i| yrow[i] * wq[o * c + i]).sum::<f32>();
            }
        }
        let erf = |x: f32| {
            // Abramowitz-Stegun 7.1.26, good to 1.5e-7
            let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
            let y = 1.0 - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_8) * t - 0.284_496_74) * t + 0.254_829_6) * t * (-x * x).exp();
            if x < 0.0 { -y } else { y }
        };
        let mut attn = vec![0f32; n * c];
        for hh in 0..heads {
            for t in 0..n {
                let qv = |tt: usize, k: usize| q[tt * c + hh * hd + k];
                let mut sc: Vec<f32> = (0..n).map(|u| 0.5 * (0..hd).map(|k| qv(t, k) * qv(u, k)).sum::<f32>()).collect();
                let m = sc.iter().cloned().fold(f32::MIN, f32::max);
                sc.iter_mut().for_each(|v| *v = (*v - m).exp());
                let z: f32 = sc.iter().sum();
                for k in 0..hd {
                    attn[t * c + hh * hd + k] = (0..n).map(|u| sc[u] / z * qv(u, k)).sum();
                }
            }
        }
        for t in 0..n {
            let row: Vec<f32> = (0..c)
                .map(|i| {
                    let a = attn[t * c + i];
                    hs[t * c + i] + 0.5 * a * (1.0 + erf(a / std::f32::consts::SQRT_2))
                })
                .collect();
            for i in 0..c {
                expect[t * c + i] = row[(i + c / 2) % c];
            }
        }
        let worst = got.data.iter().zip(&expect).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        let big = expect.iter().map(|v| v.abs()).fold(0f32, f32::max);
        assert!(worst < 0.02 * big.max(1.0), "worst difference {worst} of values up to {big}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
