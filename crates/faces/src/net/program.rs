//! A network lowered to a flat list of steps over numbered values, for an [`Accelerator`] that keeps every activation
//! on its device between layers (one upload, one read-back per run instead of one per convolution).
//!
//! Shapes are resolved for one input shape; every step names the values it reads and the one it writes, and which
//! values nothing reads after it. Shape-only operators (`Reshape`, `Flatten`, `Identity`, `Dropout`) make no step: their
//! output is the same value under another name. Anything this cannot express (a broadcast past four dimensions, a
//! transposed or batched `Gemm`, a slope that is not per channel) makes the whole lowering `None`, and the net runs
//! the CPU path instead.

use std::borrow::Cow;

use super::*;

/// Value 0 is the network's input.
pub const INPUT: usize = 0;

/// An operand: a value computed on the device, or a constant (uploaded once and kept, by `id`).
#[derive(Clone, Debug)]
pub enum Arg<'a> {
    Value(usize),
    /// `id` is the same for every run of the same net, so the device can keep the data.
    Const {
        id: usize,
        data: Cow<'a, [f32]>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Relu,
    Sigmoid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Add,
    Sub,
    Mul,
}

/// What a step computes. Element counts come from the program's `lens` of the step's output.
#[derive(Clone, Debug)]
pub enum Op<'a> {
    /// A convolution; its `input` is empty (it reads the step's first argument).
    Conv(ConvJob<'a>),
    /// `out[i] = f(x[i])`. Args: `[x]`.
    Unary(Unary),
    /// `out[i] = x[i] * scale[c] + offset[c]`, `c = (i / plane) % channels`. Args: `[x, scale, offset]`.
    ChannelAffine { channels: usize, plane: usize },
    /// `out[i] = x[i] < 0 ? x[i] * slope[c] : x[i]`. Args: `[x, slope]` (`channels` values).
    ChannelPrelu { channels: usize, plane: usize },
    /// Broadcasting over four dimensions: `dims` of the output, element strides of each operand (0 where it repeats).
    /// Args: `[a, b]`.
    Binary { op: Binary, dims: [usize; 4], a: [usize; 4], b: [usize; 4] },
    /// `out[n] = alpha · Σk x[k] · B(k, n) + bias[n]`, `B(k, n) = weights[n·k_len + k]` when `trans_b`, else
    /// `weights[k·n_len + n]`. Args: `[x, weights, bias]` (bias already times beta; zeros when the node has none).
    Gemv { k: usize, n: usize, trans_b: bool, alpha: f32 },
    /// The mean of each `plane`-sized run. Args: `[x]`.
    GlobalAverage { channels: usize, plane: usize },
    /// Unpadded max pooling. Args: `[x]`.
    MaxPool { c: usize, h: usize, w: usize, kh: usize, kw: usize, sh: usize, sw: usize, oh: usize, ow: usize },
    /// Nearest-neighbour upscale by whole numbers. Args: `[x]`.
    Resize { c: usize, h: usize, w: usize, sh: usize, sw: usize },
    /// `out` in its own order (`dims`), read from the input at `strides` (the input's strides, permuted). Args: `[x]`.
    Transpose { dims: [usize; 4], strides: [usize; 4] },
}

#[derive(Clone, Debug)]
pub struct Step<'a> {
    pub op: Op<'a>,
    pub args: Vec<Arg<'a>>,
    pub out: usize,
    /// Values nothing reads after this step (never an output): their memory can be reused.
    pub free: Vec<usize>,
}

/// A whole network for one input shape.
#[derive(Clone, Debug)]
pub struct Program<'a> {
    /// Elements of each value; `lens[INPUT]` is the input's.
    pub lens: Vec<usize>,
    pub steps: Vec<Step<'a>>,
    /// Each graph output: its name, the value that holds it and its shape.
    pub outputs: Vec<(String, usize, Vec<usize>)>,
}

/// Leading ones up to rank four, or `None` past four.
fn rank4(s: &[usize]) -> Option<[usize; 4]> {
    let pad = 4usize.checked_sub(s.len())?;
    let mut out = [1; 4];
    for (i, d) in s.iter().enumerate() {
        *out.get_mut(pad + i)? = *d;
    }
    Some(out)
}

/// Element strides of `input` read as `out` under trailing-dimension broadcasting (0 along repeated axes).
fn broadcast_strides(out: [usize; 4], input: &[usize]) -> Option<[usize; 4]> {
    let dims = rank4(input)?;
    let mut strides = [0; 4];
    let mut step = 1;
    for axis in (0..4).rev() {
        let (d, o) = (*dims.get(axis)?, *out.get(axis)?);
        if d == o && d != 1 {
            *strides.get_mut(axis)? = step;
        } else if d != 1 {
            return None;
        }
        step *= d;
    }
    Some(strides)
}

impl Net {
    /// This network lowered for `input` (its shape), or `None` when a step has no device form.
    pub fn program(&self, input: &[usize]) -> Result<Option<Program<'_>>> {
        let g = &self.graph;
        let (shapes, _) = shape::infer_all(g, input)?;
        let Some((input_name, _)) = g.inputs.first() else { return shape_err("the model has no input") };
        let mut lens = vec![shape::elements(input)?];
        let mut names: HashMap<&str, (usize, Vec<usize>)> = HashMap::new();
        names.insert(input_name.as_str(), (INPUT, input.to_vec()));
        let mut steps: Vec<Step> = Vec::new();
        for (i, node) in g.nodes.iter().enumerate() {
            let Some(out_shape) = shapes.get(i) else { return shape_err("a node has no shape") };
            let Some(out_name) = node.outputs.first() else { return shape_err("a node has no output") };
            let value = |idx: usize| -> Option<&(usize, Vec<usize>)> { node.inputs.get(idx).and_then(|n| names.get(n.as_str())) };
            let constant = |idx: usize| -> Option<(Cow<'_, [f32]>, &[usize])> {
                let w = node.inputs.get(idx).and_then(|n| g.weights.get(n))?;
                match &w.data {
                    Data::F32(v) => Some((Cow::Borrowed(v.as_slice()), w.dims.as_slice())),
                    _ => None,
                }
            };
            let id = |k: usize| i * 4 + k;
            let Some((x, x_shape)) = value(0).cloned() else { return Ok(None) };
            let (op, args) = match node.op.as_str() {
                "Reshape" | "Flatten" | "Identity" | "Dropout" => {
                    names.insert(out_name.as_str(), (x, out_shape.clone()));
                    continue;
                }
                "Conv" => (Op::Conv(kernels::conv_job(i, node, &x_shape, &[], g)?), vec![Arg::Value(x)]),
                "Relu" => (Op::Unary(Unary::Relu), vec![Arg::Value(x)]),
                "Sigmoid" => (Op::Unary(Unary::Sigmoid), vec![Arg::Value(x)]),
                "BatchNormalization" => {
                    let channels = *x_shape.get(1).ok_or_else(|| NetError::Shape("BatchNormalization rank".into()))?;
                    let plane = shape::elements(x_shape.get(2..).unwrap_or(&[]))?;
                    let (s, b, m, v) =
                        (shape::floats(node, g, 1)?, shape::floats(node, g, 2)?, shape::floats(node, g, 3)?, shape::floats(node, g, 4)?);
                    let eps = shape::float(node, "epsilon", 1e-5);
                    let mut scale = Vec::with_capacity(channels);
                    let mut offset = Vec::with_capacity(channels);
                    for c in 0..channels {
                        let (Some(s), Some(b), Some(m), Some(v)) = (s.get(c), b.get(c), m.get(c), v.get(c)) else {
                            return shape_err("short BatchNormalization parameters");
                        };
                        let k = s / (v + eps).sqrt();
                        scale.push(k);
                        offset.push(b - m * k);
                    }
                    (
                        Op::ChannelAffine { channels, plane },
                        vec![Arg::Value(x), Arg::Const { id: id(1), data: scale.into() }, Arg::Const { id: id(2), data: offset.into() }],
                    )
                }
                "PRelu" => {
                    let Some((slope, dims)) = constant(1) else { return Ok(None) };
                    let channels = x_shape.get(1).copied().unwrap_or(1);
                    let plane = shape::elements(x_shape.get(2..).unwrap_or(&[]))?;
                    let mut expanded = vec![1; x_shape.len().saturating_sub(dims.len())];
                    expanded.extend_from_slice(dims);
                    let per_channel = expanded.iter().enumerate().all(|(i, &d)| d == 1 || i == 1) && slope.len() == channels;
                    let slope: Cow<[f32]> = if per_channel {
                        slope
                    } else if let [one] = slope.as_ref() {
                        vec![*one; channels].into()
                    } else {
                        return Ok(None);
                    };
                    (Op::ChannelPrelu { channels, plane }, vec![Arg::Value(x), Arg::Const { id: id(1), data: slope }])
                }
                "Add" | "Sub" | "Mul" => {
                    let op = match node.op.as_str() {
                        "Add" => Binary::Add,
                        "Sub" => Binary::Sub,
                        _ => Binary::Mul,
                    };
                    let (b, b_shape) = match (value(1), constant(1)) {
                        (Some((v, s)), _) => (Arg::Value(*v), s.clone()),
                        (None, Some((data, dims))) => (Arg::Const { id: id(1), data }, dims.to_vec()),
                        _ => return Ok(None),
                    };
                    let Some(dims) = rank4(out_shape) else { return Ok(None) };
                    let (Some(sa), Some(sb)) = (broadcast_strides(dims, &x_shape), broadcast_strides(dims, &b_shape)) else { return Ok(None) };
                    (Op::Binary { op, dims, a: sa, b: sb }, vec![Arg::Value(x), b])
                }
                "Gemm" => {
                    let Some((weights, wdims)) = constant(1) else { return Ok(None) };
                    let ([1, k], [n_out]) = (x_shape.as_slice(), out_shape.get(1..).unwrap_or(&[])) else { return Ok(None) };
                    if node.int("transA").unwrap_or(0) != 0 || out_shape.first() != Some(&1) {
                        return Ok(None);
                    }
                    let trans_b = node.int("transB").unwrap_or(0) != 0;
                    if wdims.len() != 2 || weights.len() != k * n_out {
                        return shape_err("Gemm weights do not match");
                    }
                    let mut bias = vec![0.0f32; *n_out];
                    if node.inputs.get(2).is_some_and(|s| !s.is_empty()) {
                        let wt = shape::weight(node, g, 2)?;
                        let b = shape::floats(node, g, 2)?;
                        let beta = shape::float(node, "beta", 1.0);
                        for (j, to) in bias.iter_mut().enumerate() {
                            *to = beta
                                * b.get(kernels::broadcast_offset(j, out_shape, &wt.dims))
                                    .copied()
                                    .ok_or_else(|| NetError::Shape("short matrix bias".into()))?;
                        }
                    }
                    (
                        Op::Gemv { k: *k, n: *n_out, trans_b, alpha: shape::float(node, "alpha", 1.0) },
                        vec![Arg::Value(x), Arg::Const { id: id(1), data: weights }, Arg::Const { id: id(2), data: bias.into() }],
                    )
                }
                "GlobalAveragePool" => {
                    let (c, h, w) = chw_of(&x_shape)?;
                    (Op::GlobalAverage { channels: c, plane: h * w }, vec![Arg::Value(x)])
                }
                "MaxPool" => {
                    let (c, h, w) = chw_of(&x_shape)?;
                    let (kh, kw) = pair(node, "kernel_shape", 1)?;
                    let (sh, sw) = pair(node, "strides", 1)?;
                    if pads(node)? != (0, 0, 0, 0) {
                        return Ok(None);
                    }
                    let (_, oh, ow) = chw_of(out_shape)?;
                    (Op::MaxPool { c, h, w, kh, kw, sh, sw, oh, ow }, vec![Arg::Value(x)])
                }
                "Resize" => {
                    let (c, h, w) = chw_of(&x_shape)?;
                    let (_, oh, ow) = chw_of(out_shape)?;
                    if oh % h != 0 || ow % w != 0 {
                        return shape_err("a Resize that is not by whole numbers");
                    }
                    (Op::Resize { c, h, w, sh: oh / h, sw: ow / w }, vec![Arg::Value(x)])
                }
                "Transpose" => {
                    let rank = x_shape.len();
                    let default_perm: Vec<i64> = (0..rank as i64).rev().collect();
                    let perm = node.ints("perm").unwrap_or(&default_perm);
                    if rank > 4 || perm.len() != rank {
                        return Ok(None);
                    }
                    let mut in_strides = vec![1usize; rank];
                    for a in (0..rank.saturating_sub(1)).rev() {
                        let next = in_strides.get(a + 1).copied().unwrap_or(1) * x_shape.get(a + 1).copied().unwrap_or(1);
                        if let Some(s) = in_strides.get_mut(a) {
                            *s = next;
                        }
                    }
                    let Some(dims) = rank4(out_shape) else { return Ok(None) };
                    let mut strides = [0usize; 4];
                    for (j, p) in perm.iter().enumerate() {
                        let p = usize::try_from(*p).map_err(|_| NetError::Shape("a negative permutation".into()))?;
                        let s = in_strides.get(p).copied().ok_or_else(|| NetError::Shape("a bad permutation".into()))?;
                        *strides.get_mut(4 - rank + j).ok_or_else(|| NetError::Shape("a bad permutation".into()))? = s;
                    }
                    (Op::Transpose { dims, strides }, vec![Arg::Value(x)])
                }
                _ => return Ok(None),
            };
            let out = lens.len();
            lens.push(shape::elements(out_shape)?);
            names.insert(out_name.as_str(), (out, out_shape.clone()));
            steps.push(Step { op, args, out, free: Vec::new() });
        }
        let outputs = g
            .outputs
            .iter()
            .map(|name| {
                let (v, s) = names.get(name.as_str()).ok_or_else(|| NetError::Shape(format!("output `{name}` was never computed")))?;
                Ok((name.clone(), *v, s.clone()))
            })
            .collect::<Result<Vec<_>>>()?;
        // the last step that reads each value; a value nothing reads is free right after it is made
        let mut last: Vec<Option<usize>> = vec![None; lens.len()];
        for (at, step) in steps.iter().enumerate() {
            if let Some(l) = last.get_mut(step.out) {
                *l = Some(at);
            }
            for a in &step.args {
                if let Arg::Value(v) = a
                    && let Some(l) = last.get_mut(*v)
                {
                    *l = Some(at);
                }
            }
        }
        for (v, at) in last.iter().enumerate() {
            if outputs.iter().any(|(_, o, _)| *o == v) {
                continue;
            }
            if let Some(step) = at.and_then(|at| steps.get_mut(at)) {
                step.free.push(v);
            }
        }
        Ok(Some(Program { lens, steps, outputs }))
    }
}

pub(super) fn chw_of(s: &[usize]) -> Result<(usize, usize, usize)> {
    match s {
        [1, c, h, w] => Ok((*c, *h, *w)),
        s => unsupported(format!("a tensor of shape {s:?} (only [1, channels, height, width])")),
    }
}
