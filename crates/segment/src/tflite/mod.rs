//! A small, bounded TensorFlow Lite interpreter in pure Rust, for the official MediaPipe models
//! LightCraft runs unchanged: the multiclass selfie segmenter (`selfie_multiclass_256x256`), the
//! BlazeFace short-range face detector and the FaceMesh-V2 landmark model (`face_detector` and
//! `face_landmarks_detector` of `face_landmarker.task`), Apache-2.0, Copyright Google LLC.
//!
//! It reads the model's FlatBuffer ([`fb`], bounds-checked), dequantizes the `float16` weights
//! once at load, checks every operator's shapes, pre-arranges the convolution weights for matrix
//! products, and runs the graph in order on `f32` NHWC buffers. Only what these models use is
//! supported: tensor types `FLOAT32`, `FLOAT16` (constants, through `DEQUANTIZE`) and `INT32`
//! (constant shapes and axes), operators `CONV_2D`, `DEPTHWISE_CONV_2D`, `TRANSPOSE_CONV`, `ADD`,
//! `MUL`, `PRELU`, `RELU`, `RELU6`, `RELU_N1_TO_1`, `LOGISTIC`, `PAD`, `MAX_POOL_2D`, `RESHAPE`,
//! `TRANSPOSE`, `SOFTMAX`, `SUM`, `CONCATENATION`, `RESIZE_BILINEAR`, `RESIZE_NEAREST_NEIGHBOR` and
//! `DEQUANTIZE`, tensors of rank ≤ 4. Anything else is a clear error at load, never a panic.
//!
//! Written by the LightCraft contributors in 2026 from the TensorFlow Lite model schema
//! (`tensorflow/compiler/mlir/lite/schema/schema.fbs`: table field order, enum values and option
//! defaults; Apache-2.0, Copyright The TensorFlow Authors) and the documented behaviour of its
//! builtin operators; no TensorFlow code was copied. [`zip`] reads the stored entries of a
//! MediaPipe `.task` bundle.

mod fb;
mod ops;
pub mod zip;

use std::path::Path;

use crate::{Error, Result};
use fb::Table;
use ops::{Act, BinOp, Binary, Conv, Depthwise, MaxPool, Packed, Pad, Pool, Post, Resize, Sum, Transpose, TransposeConv, Unary, Window};

/// Largest tensor (elements): 256 MB of `f32`.
const MAX_ELEMS: usize = 64 << 20;
/// Most operators in a graph.
const MAX_OPS: usize = 8192;
/// Most tensors in a graph.
const MAX_TENSORS: usize = 1 << 16;
/// Largest model file.
const MAX_FILE: u64 = 1 << 30;
/// Most activation values over all operators (4 GB of `f32`).
const MAX_TOTAL: usize = 1 << 30;

fn bad(msg: &str) -> Error {
    Error::Message(format!("TFLite model: {msg}"))
}

// TensorType
const FLOAT32: u8 = 0;
const FLOAT16: u8 = 1;
const INT32: u8 = 2;

// BuiltinOperator
const ADD: i32 = 0;
const CONCATENATION: i32 = 2;
const CONV_2D: i32 = 3;
const DEPTHWISE_CONV_2D: i32 = 4;
const DEQUANTIZE: i32 = 6;
const LOGISTIC: i32 = 14;
const MAX_POOL_2D: i32 = 17;
const MUL: i32 = 18;
const RELU: i32 = 19;
const RELU_N1_TO_1: i32 = 20;
const RELU6: i32 = 21;
const RESHAPE: i32 = 22;
const RESIZE_BILINEAR: i32 = 23;
const SOFTMAX: i32 = 25;
const PAD: i32 = 34;
const TRANSPOSE: i32 = 39;
const PRELU: i32 = 54;
const TRANSPOSE_CONV: i32 = 67;
const SUM: i32 = 74;
const RESIZE_NEAREST_NEIGHBOR: i32 = 97;

fn op_name(code: i32) -> String {
    let n = match code {
        ADD => "ADD",
        CONCATENATION => "CONCATENATION",
        CONV_2D => "CONV_2D",
        DEPTHWISE_CONV_2D => "DEPTHWISE_CONV_2D",
        DEQUANTIZE => "DEQUANTIZE",
        LOGISTIC => "LOGISTIC",
        MAX_POOL_2D => "MAX_POOL_2D",
        MUL => "MUL",
        RELU => "RELU",
        RELU_N1_TO_1 => "RELU_N1_TO_1",
        RELU6 => "RELU6",
        RESHAPE => "RESHAPE",
        RESIZE_BILINEAR => "RESIZE_BILINEAR",
        SOFTMAX => "SOFTMAX",
        PAD => "PAD",
        TRANSPOSE => "TRANSPOSE",
        PRELU => "PRELU",
        TRANSPOSE_CONV => "TRANSPOSE_CONV",
        SUM => "SUM",
        RESIZE_NEAREST_NEIGHBOR => "RESIZE_NEAREST_NEIGHBOR",
        32 => "CUSTOM",
        c => return format!("builtin operator {c}"),
    };
    n.to_string()
}

/// `float16` bits to `f32` (exact).
fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h & 0x8000) as u32) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 => {
            // zero or subnormal: man · 2^-24
            let v = man as f32 * (1.0 / 16_777_216.0);
            return if sign != 0 { -v } else { v };
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        e => sign | ((e + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

/// A tensor of the graph as declared in the file.
struct TensorDef {
    shape: Vec<usize>,
    dtype: u8,
    name: String,
}

/// A constant tensor's values.
enum Const {
    F(Vec<f32>),
    I(Vec<i32>),
}

/// What an operator step computes.
enum Kind {
    Conv(Conv),
    Depthwise(Depthwise),
    TransposeConv(TransposeConv),
    MaxPool(MaxPool),
    Binary(Binary),
    Unary(Unary),
    Pad(Pad),
    Transpose(Transpose),
    Softmax {
        inner: usize,
        beta: f32,
    },
    Sum(Sum),
    Resize(Resize),
    Concat {
        outer: usize,
        inners: Vec<usize>,
        post: Post,
    },
    /// RESHAPE (and DEQUANTIZE of an `f32` tensor): the same values.
    Copy,
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Conv(c) if c.g.kh == 1 && c.g.kw == 1 => "conv 1x1",
            Kind::Conv(_) => "conv",
            Kind::Depthwise(_) => "depthwise",
            Kind::TransposeConv(_) => "transpose conv",
            Kind::MaxPool(_) => "max pool",
            Kind::Binary(_) => "binary",
            Kind::Unary(_) => "unary",
            Kind::Pad(_) => "pad",
            Kind::Transpose(_) => "transpose",
            Kind::Softmax { .. } => "softmax",
            Kind::Sum(_) => "sum",
            Kind::Resize(_) => "resize",
            Kind::Concat { .. } => "concat",
            Kind::Copy => "copy",
        }
    }
}

impl Kind {
    /// The post-operation a following activation or PRELU can be folded into.
    fn post_mut(&mut self) -> Option<&mut Post> {
        match self {
            Kind::Conv(k) => Some(&mut k.post),
            Kind::Depthwise(k) => Some(&mut k.post),
            Kind::TransposeConv(k) => Some(&mut k.post),
            Kind::MaxPool(k) => Some(&mut k.post),
            Kind::Binary(k) => Some(&mut k.post),
            _ => None,
        }
    }
}

struct Step {
    kind: Kind,
    /// The tensors the kernel reads (data operands only, in kernel order).
    ins: Vec<usize>,
    out: usize,
    /// Values of the output.
    len: usize,
}

/// A loaded TFLite model, ready to run (thread-safe; [`Model::run`] takes `&self`).
pub struct Model {
    tensors: Vec<TensorDef>,
    consts: Vec<Option<Vec<f32>>>,
    steps: Vec<Step>,
    inputs: Vec<usize>,
    outputs: Vec<usize>,
    /// Tensors no longer needed after each step.
    free_after: Vec<Vec<usize>>,
    pool: Pool,
}

/// Shape as 4-D (leading ones).
fn dims4(s: &[usize]) -> Result<[usize; 4]> {
    if s.len() > 4 {
        return Err(bad(&format!("tensors of rank {} are not supported (≤ 4)", s.len())));
    }
    let mut d = [1; 4];
    for (i, v) in s.iter().enumerate() {
        d[4 - s.len() + i] = *v;
    }
    Ok(d)
}

fn numel(s: &[usize]) -> Result<usize> {
    s.iter().try_fold(1usize, |a, &d| a.checked_mul(d)).filter(|&n| n <= MAX_ELEMS).ok_or_else(|| bad(&format!("tensor of shape {s:?} is too large")))
}

/// Normalize a possibly negative axis of a rank-`rank` tensor (a scalar has none).
fn axis(a: i32, rank: usize) -> Result<usize> {
    let r = rank as i64;
    let v = if a < 0 { a as i64 + r } else { a as i64 };
    if v < 0 || v >= r {
        return Err(bad(&format!("axis {a} out of range for rank {rank}")));
    }
    Ok(v as usize)
}

/// The parse/compile state.
struct Builder {
    tensors: Vec<TensorDef>,
    consts: Vec<Option<Const>>,
    /// Whether a tensor has a value at run time (graph input or produced by an earlier step).
    live: Vec<bool>,
}

impl Builder {
    fn def(&self, t: i32) -> Result<&TensorDef> {
        usize::try_from(t).ok().and_then(|t| self.tensors.get(t)).ok_or_else(|| bad(&format!("tensor index {t} out of range")))
    }

    fn shape(&self, t: i32) -> Result<Vec<usize>> {
        Ok(self.def(t)?.shape.clone())
    }

    fn floats(&self, t: i32, what: &str) -> Result<&[f32]> {
        match usize::try_from(t).ok().and_then(|t| self.consts.get(t)) {
            Some(Some(Const::F(v))) => Ok(v),
            _ => Err(bad(&format!("{what} must be a constant float tensor"))),
        }
    }

    fn ints(&self, t: i32, what: &str) -> Result<&[i32]> {
        match usize::try_from(t).ok().and_then(|t| self.consts.get(t)) {
            Some(Some(Const::I(v))) => Ok(v),
            _ => Err(bad(&format!("{what} must be a constant int32 tensor"))),
        }
    }

    /// A data operand: a live value or a float constant.
    fn data(&self, t: i32) -> Result<usize> {
        let i = usize::try_from(t).map_err(|_| bad("missing operand"))?;
        let ok = self.live.get(i).copied().unwrap_or(false) || matches!(self.consts.get(i), Some(Some(Const::F(_))));
        if !ok {
            return Err(bad(&format!("operand {i} ({}) is used before it is computed", self.def(t)?.name)));
        }
        Ok(i)
    }
}

fn input(ins: &[i32], i: usize) -> i32 {
    ins.get(i).copied().unwrap_or(-1)
}

fn usize_of(v: i32, what: &str) -> Result<usize> {
    usize::try_from(v).ok().filter(|&v| v > 0 && v <= 1 << 16).ok_or_else(|| bad(&format!("{what} {v} is not valid")))
}

/// Read `Model`'s graph from FlatBuffer bytes.
fn parse(buf: &[u8]) -> Result<Model> {
    let model = Table::root(buf)?;
    // Model: version 0, operator_codes 1, subgraphs 2, description 3, buffers 4
    let codes_v = model.vector(1, 4)?;
    let mut codes = Vec::with_capacity(codes_v.len().min(MAX_OPS));
    for i in 0..codes_v.len().min(MAX_OPS) {
        // OperatorCode: deprecated_builtin_code 0 (byte), custom_code 1, version 2, builtin_code 3 (int)
        let c = codes_v.table(i)?;
        codes.push((c.i8(0, 0)? as i32).max(c.i32(3, 0)?));
    }
    let subgraphs = model.vector(2, 4)?;
    if subgraphs.is_empty() {
        return Err(bad("no subgraph"));
    }
    let g = subgraphs.table(0)?;
    let buffers = model.vector(4, 4)?;

    // SubGraph: tensors 0, inputs 1, outputs 2, operators 3
    let tv = g.vector(0, 4)?;
    if tv.len() > MAX_TENSORS {
        return Err(bad(&format!("{} tensors (at most {MAX_TENSORS})", tv.len())));
    }
    let mut b = Builder { tensors: Vec::with_capacity(tv.len()), consts: Vec::with_capacity(tv.len()), live: vec![false; tv.len()] };
    for i in 0..tv.len() {
        // Tensor: shape 0, type 1, buffer 2, name 3
        let t = tv.table(i)?;
        let shape: Vec<usize> = t
            .vector(0, 4)?
            .i32s()?
            .into_iter()
            .map(|d| usize::try_from(d).map_err(|_| bad(&format!("tensor {i} has a dynamic or negative dimension {d}"))))
            .collect::<Result<_>>()?;
        if shape.len() > 8 {
            return Err(bad(&format!("tensor {i} has rank {}", shape.len())));
        }
        let n = numel(&shape)?;
        let dtype = t.u8(1, 0)?;
        let name = t.string(3)?;
        let bi = t.u32(2, 0)? as usize;
        if bi >= buffers.len() && bi != 0 {
            return Err(bad(&format!("tensor {name}: buffer {bi} out of range")));
        }
        let data: &[u8] = if bi == 0 {
            &[]
        } else {
            // Buffer: data 0, offset 1, size 2 (offset/size: data stored after the FlatBuffer)
            let bt = buffers.table(bi)?;
            let d = bt.vector(0, 1)?.bytes();
            let (off, size) = (bt.u64(1, 0)?, bt.u64(2, 0)?);
            if d.is_empty() && off > 1 {
                let (off, size) = (usize::try_from(off).ok(), usize::try_from(size).ok());
                off.zip(size).and_then(|(o, s)| buf.get(o..o.checked_add(s)?)).ok_or_else(|| bad(&format!("tensor {name}: data outside the file")))?
            } else {
                d
            }
        };
        let c = if data.is_empty() {
            None
        } else {
            let want = |w: usize| -> Result<()> {
                if data.len() != n * w {
                    return Err(bad(&format!("tensor {name}: {} bytes of data for shape {shape:?}", data.len())));
                }
                Ok(())
            };
            Some(match dtype {
                FLOAT32 => {
                    want(4)?;
                    Const::F(data.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
                }
                FLOAT16 => {
                    want(2)?;
                    Const::F(data.as_chunks::<2>().0.iter().map(|c| f16_to_f32(u16::from_le_bytes(*c))).collect())
                }
                INT32 => {
                    want(4)?;
                    Const::I(data.as_chunks::<4>().0.iter().map(|c| i32::from_le_bytes(*c)).collect())
                }
                t => return Err(bad(&format!("tensor {name}: type {t} is not supported (FLOAT32, FLOAT16, INT32 are)"))),
            })
        };
        b.tensors.push(TensorDef { shape, dtype, name });
        b.consts.push(c);
    }

    let index =
        |v: i32| -> Result<usize> { usize::try_from(v).ok().filter(|&v| v < tv.len()).ok_or_else(|| bad(&format!("tensor index {v} out of range"))) };
    let inputs: Vec<usize> = g.vector(1, 4)?.i32s()?.into_iter().map(index).collect::<Result<_>>()?;
    let outputs: Vec<usize> = g.vector(2, 4)?.i32s()?.into_iter().map(index).collect::<Result<_>>()?;
    for &i in &inputs {
        let t = &b.tensors[i];
        if t.dtype != FLOAT32 {
            return Err(bad(&format!("input {} is not float32", t.name)));
        }
        b.live[i] = true;
    }

    let ov = g.vector(3, 4)?;
    if ov.len() > MAX_OPS {
        return Err(bad(&format!("{} operators (at most {MAX_OPS})", ov.len())));
    }
    let mut ops = Vec::with_capacity(ov.len());
    // readers of each tensor (graph outputs count as one)
    let mut uses = vec![0usize; tv.len()];
    for &o in &outputs {
        uses[o] += 1;
    }
    for i in 0..ov.len() {
        // Operator: opcode_index 0, inputs 1, outputs 2, builtin_options_type 3, builtin_options 4
        let op = ov.table(i)?;
        let code = codes.get(op.u32(0, 0)? as usize).copied().ok_or_else(|| bad("operator code index out of range"))?;
        let ins = op.vector(1, 4)?.i32s()?;
        for &t in &ins {
            if let Some(u) = usize::try_from(t).ok().and_then(|t| uses.get_mut(t)) {
                *u += 1;
            }
        }
        ops.push((code, ins, op.vector(2, 4)?.i32s()?, op.table(4)?));
    }
    let mut steps: Vec<Step> = Vec::with_capacity(ops.len());
    for (i, (code, ins, outs, opts)) in ops.into_iter().enumerate() {
        let [out] = outs[..] else {
            return Err(bad(&format!("operator {i} ({}) has {} outputs", op_name(code), outs.len())));
        };
        let out = index(out)?;
        if b.tensors[out].dtype != FLOAT32 {
            return Err(bad(&format!("operator {i} ({}): output {} is not float32", op_name(code), b.tensors[out].name)));
        }
        if b.consts[out].is_some() || b.live[out] {
            return Err(bad(&format!("operator {i} ({}): output {} is written twice", op_name(code), b.tensors[out].name)));
        }
        let ctx = |e: Error| match e {
            Error::Message(m) => Error::Message(format!("{m} (operator {i}, {})", op_name(code))),
            e => e,
        };
        // an activation or PRELU right after the operator that is its only input folds into it
        if let Some(last) = steps.last_mut()
            && usize::try_from(input(&ins, 0)).is_ok_and(|x| x == last.out && uses[x] == 1)
            && fuse(&b, last, code, &ins, out).map_err(ctx)?
        {
            last.out = out;
            b.live[out] = true;
            continue;
        }
        let step = compile(&mut b, code, &ins, out, opts).map_err(ctx)?;
        if let Some(s) = step {
            steps.push(s);
            b.live[out] = true;
        }
    }
    for &o in &outputs {
        if !b.live[o] && !matches!(b.consts[o], Some(Const::F(_))) {
            return Err(bad(&format!("output {} is never computed", b.tensors[o].name)));
        }
    }

    // free each value after its last reader (graph outputs are kept)
    let mut last = vec![None; b.tensors.len()];
    for (si, s) in steps.iter().enumerate() {
        for &t in &s.ins {
            last[t] = Some(si);
        }
    }
    let mut free_after = vec![Vec::new(); steps.len()];
    for (t, l) in last.iter().enumerate() {
        if let Some(l) = l
            && !outputs.contains(&t)
            && b.consts[t].is_none()
        {
            free_after[*l].push(t);
        }
    }
    let total = steps.iter().try_fold(0usize, |t, s| t.checked_add(s.len)).filter(|&t| t <= MAX_TOTAL);
    if total.is_none() {
        return Err(bad("the graph's activations are too large"));
    }
    let consts = b.consts.into_iter().map(|c| if let Some(Const::F(v)) = c { Some(v) } else { None }).collect();
    Ok(Model { tensors: b.tensors, consts, steps, inputs, outputs, free_after, pool: Pool::default() })
}

/// Fold RELU / RELU6 / RELU_N1_TO_1 / PRELU into `step` (whose output is the operator's only
/// input) when its post-operation can take it.
fn fuse(b: &Builder, step: &mut Step, code: i32, ins: &[i32], out: usize) -> Result<bool> {
    let x = step.out;
    if b.tensors.get(x).map(|t| &t.shape) != b.tensors.get(out).map(|t| &t.shape) {
        return Ok(false);
    }
    let Some(post) = step.kind.post_mut() else { return Ok(false) };
    if !post.is_identity() {
        return Ok(false);
    }
    let act = match code {
        RELU => Act::Relu,
        RELU6 => Act::Relu6,
        RELU_N1_TO_1 => Act::ReluN1To1,
        PRELU => {
            // per-channel alpha only: [.., 1, C]
            let Ok(alpha) = b.floats(input(ins, 1), "alpha") else { return Ok(false) };
            let a = b.shape(input(ins, 1))?;
            let channels = b.tensors.get(x).and_then(|t| t.shape.last().copied()).unwrap_or(0);
            if a.last() != Some(&channels) || a.iter().rev().skip(1).any(|&d| d != 1) || alpha.len() != channels {
                return Ok(false);
            }
            post.alpha = Some(alpha.to_vec());
            return Ok(true);
        }
        _ => return Ok(false),
    };
    post.act = act;
    Ok(true)
}

/// Check the computed output shape against the declared one.
fn expect_shape(b: &Builder, out: usize, got: &[usize]) -> Result<()> {
    let want = &b.tensors[out].shape;
    if want.as_slice() != got {
        return Err(bad(&format!("output {} has shape {want:?}, the operator computes {got:?}", b.tensors[out].name)));
    }
    Ok(())
}

fn bias_of(b: &Builder, t: i32, n: usize) -> Result<Vec<f32>> {
    if t < 0 {
        return Ok(vec![0.0; n]);
    }
    let v = b.floats(t, "bias")?;
    if v.len() != n {
        return Err(bad(&format!("bias has {} values for {n} channels", v.len())));
    }
    Ok(v.to_vec())
}

fn opt_table<'a>(opts: Option<Table<'a>>, what: &str) -> Result<Table<'a>> {
    opts.ok_or_else(|| bad(&format!("{what} options missing")))
}

/// Padding enum: SAME = 0, VALID = 1.
fn same(p: i8) -> Result<bool> {
    match p {
        0 => Ok(true),
        1 => Ok(false),
        p => Err(bad(&format!("padding {p}"))),
    }
}

fn compile(b: &mut Builder, code: i32, ins: &[i32], out: usize, opts: Option<Table>) -> Result<Option<Step>> {
    let len = numel(&b.tensors[out].shape)?;
    let step = |kind: Kind, ins: Vec<usize>| Ok(Some(Step { kind, ins, out, len }));
    match code {
        DEQUANTIZE | RESHAPE => {
            let x = input(ins, 0);
            let n_in = numel(&b.shape(x)?)?;
            if numel(&b.tensors[out].shape)? != n_in {
                return Err(bad(&format!("{} values reshaped to {:?}", n_in, b.tensors[out].shape)));
            }
            // constants fold (the float16 weights become float32 here, once)
            if let Ok(v) = b.floats(x, "x") {
                let v = v.to_vec();
                b.consts[out] = Some(Const::F(v));
                return Ok(None);
            }
            if b.def(x)?.dtype != FLOAT32 {
                return Err(bad("only constant float16 tensors can be dequantized"));
            }
            step(Kind::Copy, vec![b.data(x)?])
        }
        CONV_2D => {
            let o = opt_table(opts, "CONV_2D")?;
            // Conv2DOptions: padding 0, stride_w 1, stride_h 2, fused_activation 3, dilation_w 4, dilation_h 5
            let (x, wt) = (input(ins, 0), input(ins, 1));
            let d = dims4(&b.shape(x)?)?;
            let ws = b.shape(wt)?;
            let [oc, kh, kw, ic] = ws[..] else { return Err(bad("weights must be [out, h, w, in]")) };
            if ic != d[3] {
                return Err(bad(&format!("weights for {ic} input channels, the input has {} (grouped convolutions are not supported)", d[3])));
            }
            let g = Window::new(
                same(o.i8(0, 0)?)?,
                d,
                (kh, kw),
                (usize_of(o.i32(2, 0)?, "stride")?, usize_of(o.i32(1, 0)?, "stride")?),
                (usize_of(o.i32(5, 1)?, "dilation")?, usize_of(o.i32(4, 1)?, "dilation")?),
            );
            let w = b.floats(wt, "CONV_2D weights")?;
            let k = kh * kw * ic;
            // [o, k] → k × o
            let conv = Conv {
                g,
                w: Packed::new(k, oc, |ki, oi| w.get(oi * k + ki).copied().unwrap_or(0.0)),
                bias: bias_of(b, input(ins, 2), oc)?,
                post: Post::act(Act::from_code(o.i8(3, 0)?)?),
            };
            expect_shape(b, out, &conv.out_shape())?;
            // the im2col matrix
            numel(&[conv.g.n, conv.g.oh, conv.g.ow, k])?;
            step(Kind::Conv(conv), vec![b.data(x)?])
        }
        DEPTHWISE_CONV_2D => {
            let o = opt_table(opts, "DEPTHWISE_CONV_2D")?;
            // DepthwiseConv2DOptions: padding 0, stride_w 1, stride_h 2, depth_multiplier 3,
            // fused_activation 4, dilation_w 5, dilation_h 6
            let (x, wt) = (input(ins, 0), input(ins, 1));
            let d = dims4(&b.shape(x)?)?;
            let ws = b.shape(wt)?;
            let [1, kh, kw, oc] = ws[..] else { return Err(bad("weights must be [1, h, w, channels]")) };
            if d[3] == 0 || !oc.is_multiple_of(d[3]) {
                return Err(bad(&format!("{oc} output channels for {} input channels", d[3])));
            }
            let dw = Depthwise {
                g: Window::new(
                    same(o.i8(0, 0)?)?,
                    d,
                    (kh, kw),
                    (usize_of(o.i32(2, 0)?, "stride")?, usize_of(o.i32(1, 0)?, "stride")?),
                    (usize_of(o.i32(6, 1)?, "dilation")?, usize_of(o.i32(5, 1)?, "dilation")?),
                ),
                mult: oc / d[3],
                w: b.floats(wt, "DEPTHWISE_CONV_2D weights")?.to_vec(),
                bias: bias_of(b, input(ins, 2), oc)?,
                post: Post::act(Act::from_code(o.i8(4, 0)?)?),
            };
            expect_shape(b, out, &dw.out_shape())?;
            step(Kind::Depthwise(dw), vec![b.data(x)?])
        }
        TRANSPOSE_CONV => {
            let o = opt_table(opts, "TRANSPOSE_CONV")?;
            // TransposeConvOptions: padding 0, stride_w 1, stride_h 2, fused_activation 3
            // inputs: output_shape, weights [o, h, w, in], input, bias (optional)
            let (shape_t, wt, x) = (input(ins, 0), input(ins, 1), input(ins, 2));
            let os: Vec<usize> = b.ints(shape_t, "TRANSPOSE_CONV output shape")?.iter().map(|&v| usize::try_from(v).unwrap_or(0)).collect();
            let [n, oh, ow, oc] = os[..] else { return Err(bad("output shape must have 4 values")) };
            let d = dims4(&b.shape(x)?)?;
            let ws = b.shape(wt)?;
            let [woc, kh, kw, ic] = ws[..] else { return Err(bad("weights must be [out, h, w, in]")) };
            if woc != oc || ic != d[3] || n != d[0] {
                return Err(bad(&format!("weights {ws:?} don't fit input {d:?} and output {os:?}")));
            }
            let (sh, sw) = (usize_of(o.i32(2, 0)?, "stride")?, usize_of(o.i32(1, 0)?, "stride")?);
            let same = same(o.i8(0, 0)?)?;
            // the padding of the forward convolution from the output back to the input
            let (_, pt) = ops::pad_before(same, oh, kh, sh, 1);
            let (_, pl) = ops::pad_before(same, ow, kw, sw, 1);
            let w = b.floats(wt, "TRANSPOSE_CONV weights")?;
            // [o, kh, kw, in] → in × (kh, kw, o)
            let khw = kh * kw;
            let weights = Packed::new(ic, khw * oc, |ci, col| {
                let (tap, oi) = (col / oc.max(1), col % oc.max(1));
                w.get((oi * khw + tap) * ic + ci).copied().unwrap_or(0.0)
            });
            let tc = TransposeConv {
                n,
                h: d[1],
                w: d[2],
                oh,
                ow,
                o: oc,
                kh,
                kw,
                sh,
                sw,
                pt,
                pl,
                weights,
                bias: bias_of(b, input(ins, 3), oc)?,
                post: Post::act(Act::from_code(o.i8(3, 0)?)?),
            };
            numel(&os)?;
            // the product before the scatter
            numel(&[n, d[1], d[2], kh * kw * oc])?;
            expect_shape(b, out, &tc.out_shape())?;
            step(Kind::TransposeConv(tc), vec![b.data(x)?])
        }
        MAX_POOL_2D => {
            let o = opt_table(opts, "MAX_POOL_2D")?;
            // Pool2DOptions: padding 0, stride_w 1, stride_h 2, filter_w 3, filter_h 4, fused_activation 5
            let x = input(ins, 0);
            let d = dims4(&b.shape(x)?)?;
            let mp = MaxPool {
                g: Window::new(
                    same(o.i8(0, 0)?)?,
                    d,
                    (usize_of(o.i32(4, 0)?, "filter")?, usize_of(o.i32(3, 0)?, "filter")?),
                    (usize_of(o.i32(2, 0)?, "stride")?, usize_of(o.i32(1, 0)?, "stride")?),
                    (1, 1),
                ),
                post: Post::act(Act::from_code(o.i8(5, 0)?)?),
            };
            expect_shape(b, out, &mp.out_shape())?;
            step(Kind::MaxPool(mp), vec![b.data(x)?])
        }
        ADD | MUL | PRELU => {
            // AddOptions / MulOptions: fused_activation 0
            let act = match opts {
                Some(o) if code != PRELU => Act::from_code(o.i8(0, 0)?)?,
                _ => Act::None,
            };
            let (mut x, mut y) = (input(ins, 0), input(ins, 1));
            // commutative: put the full-size operand first (the fast path)
            if code != PRELU && numel(&b.shape(x)?)? < numel(&b.shape(y)?)? {
                std::mem::swap(&mut x, &mut y);
            }
            let (sx, sy) = (b.shape(x)?, b.shape(y)?);
            let (a, c) = (dims4(&sx)?, dims4(&sy)?);
            let shape = ops::broadcast(a, c)?;
            let rank = sx.len().max(sy.len());
            expect_shape(b, out, shape.get(4 - rank..).unwrap_or(&[]))?;
            let op = match code {
                ADD => BinOp::Add,
                MUL => BinOp::Mul,
                _ => BinOp::Prelu,
            };
            step(Kind::Binary(Binary { op, a, b: c, out: shape, post: Post::act(act) }), vec![b.data(x)?, b.data(y)?])
        }
        RELU | RELU6 | RELU_N1_TO_1 | LOGISTIC => {
            let x = input(ins, 0);
            expect_shape(b, out, &b.shape(x)?)?;
            let u = match code {
                RELU => Unary::Act(Act::Relu),
                RELU6 => Unary::Act(Act::Relu6),
                RELU_N1_TO_1 => Unary::Act(Act::ReluN1To1),
                _ => Unary::Logistic,
            };
            step(Kind::Unary(u), vec![b.data(x)?])
        }
        PAD => {
            let (x, p) = (input(ins, 0), input(ins, 1));
            let s = b.shape(x)?;
            let pads = b.ints(p, "PAD paddings")?;
            if pads.len() != 2 * s.len() || pads.iter().any(|&v| !(0..=1 << 16).contains(&v)) {
                return Err(bad(&format!("paddings {pads:?} for rank {}", s.len())));
            }
            let lead = 4usize.saturating_sub(s.len());
            let d = dims4(&s)?;
            let mut before = [0; 4];
            let mut outd = d;
            for i in 0..s.len() {
                let (lo, hi) = (pads.get(2 * i).copied().unwrap_or(0) as usize, pads.get(2 * i + 1).copied().unwrap_or(0) as usize);
                before[lead + i] = lo;
                outd[lead + i] += lo + hi;
            }
            numel(&outd)?;
            expect_shape(b, out, outd.get(lead..).unwrap_or(&[]))?;
            step(Kind::Pad(Pad { d, before, out: outd }), vec![b.data(x)?])
        }
        TRANSPOSE => {
            let (x, p) = (input(ins, 0), input(ins, 1));
            let s = b.shape(x)?;
            let perm = b.ints(p, "TRANSPOSE permutation")?;
            let d = dims4(&s)?;
            let lead = 4usize.saturating_sub(s.len());
            let mut p4 = [0, 1, 2, 3];
            let mut seen = [false; 4];
            if perm.len() != s.len() {
                return Err(bad(&format!("permutation {perm:?} for rank {}", s.len())));
            }
            for (i, &v) in perm.iter().enumerate() {
                let a = axis(v, s.len())? + lead;
                if std::mem::replace(&mut seen[a], true) {
                    return Err(bad(&format!("permutation {perm:?} repeats an axis")));
                }
                p4[lead + i] = a;
            }
            let t = Transpose { d, perm: p4 };
            expect_shape(b, out, t.out_shape().get(lead..).unwrap_or(&[]))?;
            step(Kind::Transpose(t), vec![b.data(x)?])
        }
        SOFTMAX => {
            // SoftmaxOptions: beta 0
            let beta = opt_table(opts, "SOFTMAX")?.f32(0, 0.0)?;
            let x = input(ins, 0);
            let s = b.shape(x)?;
            expect_shape(b, out, &s)?;
            step(Kind::Softmax { inner: s.last().copied().unwrap_or(1), beta }, vec![b.data(x)?])
        }
        SUM => {
            // ReducerOptions: keep_dims 0
            let keep = opts.map(|o| o.bool(0)).transpose()?.unwrap_or(false);
            let (x, a) = (input(ins, 0), input(ins, 1));
            let s = b.shape(x)?;
            let d = dims4(&s)?;
            let lead = 4usize.saturating_sub(s.len());
            let mut reduce = [false; 4];
            for &v in b.ints(a, "SUM axes")? {
                reduce[axis(v, s.len())? + lead] = true;
            }
            let shape: Vec<usize> = s
                .iter()
                .enumerate()
                .filter_map(|(i, &d)| match (reduce[lead + i], keep) {
                    (true, true) => Some(1),
                    (true, false) => None,
                    (false, _) => Some(d),
                })
                .collect();
            expect_shape(b, out, &shape)?;
            step(Kind::Sum(Sum { d, reduce }), vec![b.data(x)?])
        }
        RESIZE_BILINEAR | RESIZE_NEAREST_NEIGHBOR => {
            // ResizeBilinearOptions: new_height 0, new_width 1 (deprecated), align_corners 2,
            // half_pixel_centers 3; ResizeNearestNeighborOptions: align_corners 0, half_pixel_centers 1
            let bilinear = code == RESIZE_BILINEAR;
            let (ac, hp) = if bilinear { (2, 3) } else { (0, 1) };
            let (align_corners, half_pixel) = match opts {
                Some(o) => (o.bool(ac)?, o.bool(hp)?),
                None => (false, false),
            };
            let (x, sz) = (input(ins, 0), input(ins, 1));
            let s = b.ints(sz, "resize size")?;
            let [oh, ow] = s[..] else { return Err(bad("resize size must have 2 values")) };
            let d = b.shape(x)?;
            let [n, h, w, c] = d[..] else { return Err(bad("resize input must be 4-D")) };
            let r = Resize { n, h, w, c, oh: usize_of(oh, "size")?, ow: usize_of(ow, "size")?, bilinear, align_corners, half_pixel };
            numel(&r.out_shape())?;
            expect_shape(b, out, &r.out_shape())?;
            step(Kind::Resize(r), vec![b.data(x)?])
        }
        CONCATENATION => {
            let o = opt_table(opts, "CONCATENATION")?;
            // ConcatenationOptions: axis 0, fused_activation 1
            let first = b.shape(input(ins, 0))?;
            let ax = axis(o.i32(0, 0)?, first.len())?;
            let outer: usize = first.get(..ax).unwrap_or(&[]).iter().product();
            let mut shape = first.clone();
            shape[ax] = 0;
            let mut inners = Vec::new();
            let mut data = Vec::new();
            for &t in ins {
                let s = b.shape(t)?;
                if s.len() != first.len() || s.iter().zip(&first).enumerate().any(|(i, (a, f))| i != ax && a != f) {
                    return Err(bad(&format!("can't concatenate {s:?} with {first:?} on axis {ax}")));
                }
                shape[ax] += s[ax];
                inners.push(s.get(ax..).unwrap_or(&[]).iter().product());
                data.push(b.data(t)?);
            }
            expect_shape(b, out, &shape)?;
            step(Kind::Concat { outer, inners, post: Post::act(Act::from_code(o.i8(1, 0)?)?) }, data)
        }
        c => Err(bad(&format!("operator {} is not supported", op_name(c)))),
    }
}

impl Model {
    /// Parse and validate a `.tflite` FlatBuffer, dequantize its constants and prepare its
    /// weights. Errors (never panics) on a malformed file or an unsupported operator.
    pub fn from_bytes(bytes: &[u8]) -> Result<Model> {
        parse(bytes)
    }

    /// [`Model::from_bytes`] on a file.
    pub fn load(path: &Path) -> Result<Model> {
        let len = std::fs::metadata(path).map_err(|e| Error::Message(format!("can't read {}: {e}", path.display())))?.len();
        if len > MAX_FILE {
            return Err(bad(&format!("{} is too large ({len} bytes)", path.display())));
        }
        let bytes = std::fs::read(path).map_err(|e| Error::Message(format!("can't read {}: {e}", path.display())))?;
        Self::from_bytes(&bytes).map_err(|e| Error::Message(format!("{}: {e}", path.display())))
    }

    /// The shapes of the graph inputs (NHWC for images).
    pub fn input_shapes(&self) -> Vec<Vec<usize>> {
        self.inputs.iter().map(|&i| self.tensors.get(i).map(|t| t.shape.clone()).unwrap_or_default()).collect()
    }

    /// The shapes of the graph outputs, in model order.
    pub fn output_shapes(&self) -> Vec<Vec<usize>> {
        self.outputs.iter().map(|&i| self.tensors.get(i).map(|t| t.shape.clone()).unwrap_or_default()).collect()
    }

    /// Run the graph: one row-major `f32` buffer per input (in model order, NHWC), the outputs in
    /// model order.
    pub fn run(&self, inputs: &[&[f32]]) -> Result<Vec<Vec<f32>>> {
        let mut vals = self.execute(inputs, false, None)?;
        let mut outs: Vec<Vec<f32>> = Vec::with_capacity(self.outputs.len());
        for (i, &o) in self.outputs.iter().enumerate() {
            let v = match vals.get_mut(o).and_then(Option::take) {
                Some(v) => v,
                None => {
                    // a constant output, or one listed twice
                    let earlier = self.outputs.iter().take(i).position(|&p| p == o).and_then(|j| outs.get(j));
                    earlier.or_else(|| self.consts.get(o).and_then(|c| c.as_ref())).ok_or_else(|| bad("an output was not computed"))?.clone()
                }
            };
            outs.push(v);
        }
        for v in vals.into_iter().flatten() {
            self.pool.give(v);
        }
        Ok(outs)
    }

    /// Every computed tensor by name (for comparisons with a reference interpreter).
    #[doc(hidden)]
    pub fn run_traced(&self, inputs: &[&[f32]]) -> Result<Vec<(String, Vec<f32>)>> {
        let vals = self.execute(inputs, true, None)?;
        Ok(vals.into_iter().zip(&self.tensors).filter_map(|(v, t)| Some((t.name.clone(), v?))).collect())
    }

    /// Time spent per operator kind in one run (for tuning).
    #[doc(hidden)]
    pub fn profile(&self, inputs: &[&[f32]]) -> Result<Vec<(&'static str, std::time::Duration)>> {
        let mut p = Vec::new();
        self.execute(inputs, false, Some(&mut p))?;
        let mut total: Vec<(&'static str, std::time::Duration)> = Vec::new();
        for (k, d) in p {
            match total.iter_mut().find(|(n, _)| *n == k) {
                Some(t) => t.1 += d,
                None => total.push((k, d)),
            }
        }
        total.sort_by_key(|t| std::cmp::Reverse(t.1));
        Ok(total)
    }

    fn execute(
        &self,
        inputs: &[&[f32]],
        keep: bool,
        mut prof: Option<&mut Vec<(&'static str, std::time::Duration)>>,
    ) -> Result<Vec<Option<Vec<f32>>>> {
        if inputs.len() != self.inputs.len() {
            return Err(bad(&format!("{} inputs given, the model takes {}", inputs.len(), self.inputs.len())));
        }
        let mut vals: Vec<Option<Vec<f32>>> = vec![None; self.tensors.len()];
        for (&t, x) in self.inputs.iter().zip(inputs) {
            let def = self.tensors.get(t).ok_or_else(|| bad("input index"))?;
            let n = numel(&def.shape)?;
            if x.len() != n {
                return Err(bad(&format!("input {} takes {n} values ({:?}), {} given", def.name, def.shape, x.len())));
            }
            let mut v = self.pool.take(n);
            v.copy_from_slice(x);
            if let Some(slot) = vals.get_mut(t) {
                *slot = Some(v);
            }
        }
        for (si, s) in self.steps.iter().enumerate() {
            let frees = self.free_after.get(si).map(|v| v.as_slice()).unwrap_or(&[]);
            // a reshape of a value read for the last time moves it
            if let (Kind::Copy, Some(&x)) = (&s.kind, s.ins.first())
                && !keep
                && frees.contains(&x)
                && let Some(v) = vals.get_mut(x).and_then(Option::take)
            {
                if let Some(slot) = vals.get_mut(s.out) {
                    *slot = Some(v);
                }
                continue;
            }
            let mut y = self.pool.take(s.len);
            let get = |t: usize| -> Result<&[f32]> {
                vals.get(t)
                    .and_then(|v| v.as_deref())
                    .or_else(|| self.consts.get(t).and_then(|c| c.as_deref()))
                    .ok_or_else(|| bad(&format!("tensor {t} has no value")))
            };
            let x = get(*s.ins.first().ok_or_else(|| bad("operator without input"))?)?;
            let t0 = std::time::Instant::now();
            let o = &mut y;
            match &s.kind {
                Kind::Conv(k) => k.run(x, o, &self.pool)?,
                Kind::Depthwise(k) => k.run(x, o)?,
                Kind::TransposeConv(k) => k.run(x, o, &self.pool)?,
                Kind::MaxPool(k) => k.run(x, o)?,
                Kind::Binary(k) => k.run(x, get(*s.ins.get(1).ok_or_else(|| bad("missing operand"))?)?, o)?,
                Kind::Unary(k) => k.run(x, o)?,
                Kind::Pad(k) => k.run(x, o)?,
                Kind::Transpose(k) => k.run(x, o)?,
                Kind::Softmax { inner, beta } => ops::softmax(x, *inner, *beta, o)?,
                Kind::Sum(k) => k.run(x, o)?,
                Kind::Resize(k) => k.run(x, o)?,
                Kind::Concat { outer, inners, post } => {
                    let xs = s.ins.iter().map(|&t| get(t)).collect::<Result<Vec<_>>>()?;
                    ops::concat(&xs, *outer, inners, post, o)?
                }
                Kind::Copy => {
                    if x.len() != o.len() {
                        return Err(bad("reshape size"));
                    }
                    o.copy_from_slice(x);
                }
            }
            if let Some(p) = prof.as_deref_mut() {
                p.push((s.kind.name(), t0.elapsed()));
            }
            if let Some(v) = vals.get_mut(s.out) {
                *v = Some(y);
            }
            if !keep {
                for &t in frees {
                    if let Some(v) = vals.get_mut(t).and_then(Option::take) {
                        self.pool.give(v);
                    }
                }
            }
        }
        Ok(vals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_floats_convert_exactly() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x3555), 0.333_251_95);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(f16_to_f32(0x8000), -0.0);
        assert!(f16_to_f32(0x7c00).is_infinite());
        assert!(f16_to_f32(0x7e00).is_nan());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(Model::from_bytes(&[]).is_err());
        assert!(Model::from_bytes(&[0; 64]).is_err());
        let noise: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
        assert!(Model::from_bytes(&noise).is_err());
        assert!(Model::load(Path::new("/nonexistent/model.tflite")).is_err());
    }

    /// A FlatBuffer value for the tiny test writer below.
    enum V {
        U8(u8),
        I32(i32),
        U32(u32),
        Ints(Vec<i32>),
        Bytes(Vec<u8>),
        Str(&'static str),
        T(Vec<Option<V>>),
        Ts(Vec<Vec<Option<V>>>),
    }

    /// Writes tables front to back: vtable, table (every field in a 4-byte slot), then the
    /// objects its offsets point to (offsets always point forward, as the format requires).
    struct Writer(Vec<u8>);

    impl Writer {
        fn align(&mut self) {
            while !self.0.len().is_multiple_of(4) {
                self.0.push(0);
            }
        }

        fn patch(&mut self, at: usize, target: usize) {
            self.0[at..at + 4].copy_from_slice(&((target - at) as u32).to_le_bytes());
        }

        fn table(&mut self, fields: &[Option<V>]) -> usize {
            self.align();
            let vt = self.0.len();
            let n = fields.len();
            self.0.extend(((4 + 2 * n) as u16).to_le_bytes());
            self.0.extend(((4 + 4 * n) as u16).to_le_bytes());
            for (i, f) in fields.iter().enumerate() {
                self.0.extend((if f.is_some() { 4 + 4 * i as u16 } else { 0 }).to_le_bytes());
            }
            self.align();
            let pos = self.0.len();
            self.0.extend(((pos - vt) as i32).to_le_bytes());
            let mut pending = Vec::new();
            for f in fields {
                let at = self.0.len();
                match f {
                    None => self.0.extend([0; 4]),
                    Some(V::U8(v)) => self.0.extend([*v, 0, 0, 0]),
                    Some(V::I32(v)) => self.0.extend(v.to_le_bytes()),
                    Some(V::U32(v)) => self.0.extend(v.to_le_bytes()),
                    Some(v) => {
                        self.0.extend([0; 4]);
                        pending.push((at, v));
                    }
                }
            }
            for (at, v) in pending {
                let target = self.object(v);
                self.patch(at, target);
            }
            pos
        }

        fn object(&mut self, v: &V) -> usize {
            self.align();
            let p = self.0.len();
            match v {
                V::Ints(xs) => {
                    self.0.extend((xs.len() as u32).to_le_bytes());
                    xs.iter().for_each(|x| self.0.extend(x.to_le_bytes()));
                }
                V::Bytes(b) => {
                    self.0.extend((b.len() as u32).to_le_bytes());
                    self.0.extend(b);
                }
                V::Str(s) => {
                    self.0.extend((s.len() as u32).to_le_bytes());
                    self.0.extend(s.as_bytes());
                    self.0.push(0);
                }
                V::T(f) => return self.table(f),
                V::Ts(ts) => {
                    self.0.extend((ts.len() as u32).to_le_bytes());
                    let slots: Vec<usize> = ts
                        .iter()
                        .map(|_| {
                            let at = self.0.len();
                            self.0.extend([0; 4]);
                            at
                        })
                        .collect();
                    for (t, at) in ts.iter().zip(slots) {
                        let target = self.table(t);
                        self.patch(at, target);
                    }
                }
                _ => panic!("scalars live in tables"),
            }
            p
        }
    }

    fn build(root: &[Option<V>]) -> Vec<u8> {
        let mut w = Writer(vec![0; 4]);
        let r = w.table(root);
        w.patch(0, r);
        w.0
    }

    fn tensor(shape: &[i32], dtype: u8, buffer: u32, name: &'static str) -> Vec<Option<V>> {
        vec![Some(V::Ints(shape.to_vec())), Some(V::U8(dtype)), Some(V::U32(buffer)), Some(V::Str(name))]
    }

    fn op(code_index: u32, ins: &[i32], outs: &[i32], options: Option<Vec<Option<V>>>) -> Vec<Option<V>> {
        vec![Some(V::U32(code_index)), Some(V::Ints(ins.to_vec())), Some(V::Ints(outs.to_vec())), None, options.map(V::T)]
    }

    /// float16 bits of 0, ±0.5, ±1, ±2.
    fn half(v: f32) -> u16 {
        let b = match v.abs() {
            0.0 => 0,
            0.5 => 0x3800,
            1.0 => 0x3c00,
            2.0 => 0x4000,
            _ => panic!("no float16 bits for {v}"),
        };
        if v < 0.0 { b | 0x8000 } else { b }
    }

    const WEIGHTS: [f32; 6] = [1.0, -0.5, 2.0, 0.0, -1.0, 0.5];

    /// input [1, 4, 4, 2] → CONV_2D (float16 weights through DEQUANTIZE, 3 × 3, SAME, stride 2)
    /// → RELU → RESHAPE [1, 12]; `conv_code` replaces CONV_2D's operator code.
    fn tiny_model(conv_code: i32) -> Vec<u8> {
        let w16: Vec<u8> = (0..54).flat_map(|i| half(WEIGHTS[i % 6]).to_le_bytes()).collect();
        let bias: Vec<u8> = [0.5f32, -1.0, 0.25].iter().flat_map(|v| v.to_le_bytes()).collect();
        let shape: Vec<u8> = [1i32, 12].iter().flat_map(|v| v.to_le_bytes()).collect();
        let code = |c: i32| vec![Some(V::U8(c.min(127) as u8)), None, Some(V::I32(1)), Some(V::I32(c))];
        let graph = vec![
            Some(V::Ts(vec![
                tensor(&[1, 4, 4, 2], FLOAT32, 0, "input"),
                tensor(&[3, 3, 3, 2], FLOAT16, 1, "w16"),
                tensor(&[3, 3, 3, 2], FLOAT32, 0, "w"),
                tensor(&[3], FLOAT32, 2, "bias"),
                tensor(&[1, 2, 2, 3], FLOAT32, 0, "conv"),
                tensor(&[1, 2, 2, 3], FLOAT32, 0, "relu"),
                tensor(&[2], INT32, 3, "shape"),
                tensor(&[1, 12], FLOAT32, 0, "output"),
            ])),
            Some(V::Ints(vec![0])),
            Some(V::Ints(vec![7])),
            Some(V::Ts(vec![
                op(0, &[1], &[2], None),
                // Conv2DOptions: SAME, stride 2 × 2, no activation
                op(1, &[0, 2, 3], &[4], Some(vec![Some(V::U8(0)), Some(V::I32(2)), Some(V::I32(2)), Some(V::U8(0))])),
                op(2, &[4], &[5], None),
                op(3, &[5, 6], &[7], None),
            ])),
        ];
        let empty = vec![];
        build(&[
            Some(V::U32(3)),
            Some(V::Ts(vec![code(DEQUANTIZE), code(conv_code), code(RELU), code(RESHAPE)])),
            Some(V::Ts(vec![graph])),
            Some(V::Str("test")),
            Some(V::Ts(vec![empty, vec![Some(V::Bytes(w16))], vec![Some(V::Bytes(bias))], vec![Some(V::Bytes(shape))]])),
        ])
    }

    fn tiny_input() -> Vec<f32> {
        (0..32).map(|i| i as f32 * 0.1 - 1.0).collect()
    }

    /// The tiny model by hand: SAME 4 → 2 with k = 3, s = 2 pads 0 before, 1 after.
    fn tiny_expected() -> Vec<f32> {
        let x = tiny_input();
        let bias = [0.5f32, -1.0, 0.25];
        let mut out = Vec::new();
        for oy in 0..2 {
            for ox in 0..2 {
                for o in 0..3 {
                    let mut acc = bias[o];
                    for ky in 0..3 {
                        for kx in 0..3 {
                            let (iy, ix) = (oy * 2 + ky, ox * 2 + kx);
                            if iy >= 4 || ix >= 4 {
                                continue;
                            }
                            for c in 0..2 {
                                acc += x[(iy * 4 + ix) * 2 + c] * WEIGHTS[(((o * 3 + ky) * 3 + kx) * 2 + c) % 6];
                            }
                        }
                    }
                    out.push(acc.max(0.0));
                }
            }
        }
        out
    }

    #[test]
    fn runs_a_hand_built_model() {
        let m = Model::from_bytes(&tiny_model(CONV_2D)).unwrap();
        assert_eq!(m.input_shapes(), vec![vec![1, 4, 4, 2]]);
        assert_eq!(m.output_shapes(), vec![vec![1, 12]]);
        // DEQUANTIZE folded at load, RELU fused into the convolution, RESHAPE moves
        assert_eq!(m.steps.len(), 2);
        let want = tiny_expected();
        assert!(want.contains(&0.0) && want.iter().any(|v| *v > 0.0));
        for _ in 0..3 {
            // repeated runs reuse pooled (stale) buffers
            let out = m.run(&[&tiny_input()]).unwrap();
            assert_eq!(out.len(), 1);
            for (a, b) in out[0].iter().zip(&want) {
                assert!((a - b).abs() < 1e-5, "{a} vs {b}");
            }
        }
        // the traced run names every computed tensor
        let trace = m.run_traced(&[&tiny_input()]).unwrap();
        assert!(trace.iter().any(|(n, _)| n == "output"));
        // wrong inputs are errors
        assert!(m.run(&[]).is_err());
        assert!(m.run(&[&[0.0; 3]]).is_err());
    }

    #[test]
    fn unsupported_operators_are_named() {
        // FULLY_CONNECTED (9) in place of CONV_2D
        let e = Model::from_bytes(&tiny_model(9)).err().unwrap().to_string();
        assert!(e.contains("not supported") && e.contains("builtin operator 9"), "{e}");
    }

    #[test]
    fn damaged_models_never_panic() {
        let good = tiny_model(CONV_2D);
        let x = tiny_input();
        let mut loaded = 0;
        let mut check = |bytes: &[u8]| {
            if let Ok(m) = Model::from_bytes(bytes) {
                loaded += 1;
                let shapes = m.input_shapes();
                let inputs: Vec<Vec<f32>> = shapes.iter().map(|s| x.iter().copied().cycle().take(s.iter().product()).collect()).collect();
                let refs: Vec<&[f32]> = inputs.iter().map(|v| v.as_slice()).collect();
                let _ = m.run(&refs);
            }
        };
        for n in 0..good.len() {
            check(&good[..n]);
        }
        for i in 0..good.len() {
            for flip in [0xff, 0x01, 0x80] {
                let mut bad = good.clone();
                bad[i] ^= flip;
                check(&bad);
            }
        }
        // some damage leaves a loadable model (e.g. weight bytes), most doesn't
        assert!(loaded > 0);
    }

    /// A scalar (rank-0) tensor has no axis: SUM and CONCATENATION over one are refused at load
    /// (they indexed out of bounds before).
    #[test]
    fn axes_of_a_scalar_are_refused() {
        let axes = 0i32.to_le_bytes().to_vec();
        for (code, ins, options) in [(SUM, vec![0, 1], None), (CONCATENATION, vec![0, 0], Some(vec![Some(V::I32(0)), Some(V::U8(0))]))] {
            let graph = vec![
                Some(V::Ts(vec![tensor(&[], FLOAT32, 0, "x"), tensor(&[1], INT32, 1, "axes"), tensor(&[], FLOAT32, 0, "y")])),
                Some(V::Ints(vec![0])),
                Some(V::Ints(vec![2])),
                Some(V::Ts(vec![op(0, &ins, &[2], options)])),
            ];
            let model = build(&[
                Some(V::U32(3)),
                Some(V::Ts(vec![vec![Some(V::U8(code.min(127) as u8)), None, Some(V::I32(1)), Some(V::I32(code))]])),
                Some(V::Ts(vec![graph])),
                Some(V::Str("test")),
                Some(V::Ts(vec![vec![], vec![Some(V::Bytes(axes.clone()))]])),
            ]);
            let e = Model::from_bytes(&model).err().map(|e| e.to_string());
            assert!(e.as_deref().is_some_and(|e| e.contains("axis")), "{code}: {e:?}");
        }
    }
}
