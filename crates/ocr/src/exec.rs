//! Executes an ONNX graph's operators on candle: the operators of PaddleOCR's exported PP-OCRv6 detector and
//! recogniser and its PP-LCNet text-line orientation classifier (opsets 7 to 17).
//!
//! Convolutions (`auto_pad` SAME_UPPER, unequal strides, groups), transposed convolutions, BatchNormalization,
//! the activations (Relu, Sigmoid, HardSigmoid, Erf), element-wise arithmetic with broadcasting (Add, Sub, Mul, Div,
//! Pow, Sqrt), ReduceMean, Softmax, MatMul, pooling, nearest Resize, the layout operators (Transpose, Reshape, Squeeze,
//! Unsqueeze, Concat, Slice) and the shape arithmetic (Shape, Gather, Slice and Concat on int64 vectors) that feeds
//! Reshape its sizes; Constant nodes become constants at load. Unsupported operators, attributes and dangling value
//! names fail at load, not in the middle of a run. Every operator that allocates a size the model file controls (see
//! `MAX_RUNTIME_ELEMENTS`) checks it before candle does, and a run may hold at most `MAX_LIVE_ELEMENTS` at once: its
//! values (the input, results not yet freed, retained outputs) and, while an operator runs, every buffer that operator
//! holds with them (a padded copy of its input, scratch, its output). A `Budget` made by each run counts both, so a
//! hostile model is an error and never an out-of-memory abort.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use candle_core::{Device, Tensor};

use crate::onnx::{self, Attr, Data, ModelProto, NodeProto, TensorProto};
use crate::{Error, Result};

/// Largest tensor any operator may produce at run time, in elements (2^30, 4 GiB of f32): its output and the copies it
/// makes of an operand on the way (a padded copy, a stride-1 intermediate, a broadcast operand). Every operator checks
/// the sizes it is about to ask candle for with [`checked_elements`] before asking, because those sizes come from the
/// model file (weights, constants, shape arithmetic) and not from the input image. Operators whose output is no larger
/// than an input are exempt.
///
/// Why this number: the reader feeds the detector up to 4000 × 4000 pixels, and the largest feature map the shipped
/// models (PP-OCRv6 tiny, small, medium) build there is the medium detector's first padded convolution input,
/// 128 × 2001 × 2001 = 512 million elements. 2^30 is 2.1 times that, so every shipped model passes with room to spare
/// at the largest photo, and a model that asks for more (two tiny initializers broadcast to 16 GB) is an error.
const MAX_RUNTIME_ELEMENTS: usize = 1 << 30;
/// Largest scratch buffer a convolution may build, in elements (2^32, 16 GiB of f32): the im2col buffer of a convolution
/// (one row of `c_in · kh · kw` taps per output position) and the column buffer of a transposed convolution. Scratch is
/// much larger than the output it makes (it holds `c_in · kh · kw` values per output position, where the output holds
/// `c_out`), so it has its own, higher ceiling than [`MAX_RUNTIME_ELEMENTS`].
///
/// Why this number: at the same 4000 × 4000 photo the medium detector's widest 3 × 3 convolution (Conv.121, 256 channels
/// in, 1000 × 1000 out) builds 1 × 1000 × 1000 × 256 × 3 × 3 = 2.304 billion elements; 2^32 is 1.86 times that. (A
/// 1600 × 1216 photo needs 280 million of them.) The tiny and
/// small sets, the recogniser (6 lines of 48 × 3200) and the orientation classifier (32 lines of 80 × 160) all need less.
/// A model that asks for more is an error, not an out-of-memory abort.
const MAX_SCRATCH_ELEMENTS: usize = 1 << 32;
/// Most elements one run may hold at once (4.5 billion, 16.8 GiB of f32): the input, every value computed and not yet
/// freed (graph outputs are never freed, so a run keeps all of them), and, while an operator runs, every buffer it holds
/// at the same time: a padded copy of its input, the input laid out for candle, the scratch, the output and the
/// intermediates that exist with them. `Budget` counts all of these in one total and refuses a request that would take it
/// over the limit before the buffer is built; the buffers of an operator are given back when it finishes or fails, and
/// its result is then counted as one of the run's values. A model that repeats one cheap node and keeps every result is
/// an error naming the node, not an out-of-memory abort. Shared storage (a Reshape of a value) counts once per name:
/// the total is an upper bound. Constants (initializers) are not counted: they are bounded by the size of the model file.
///
/// Why this number: it is the most the shipped models hold at once, measured, plus a margin. The same accounting run
/// on the real graphs at the reader's largest 4000 × 4000 photo gives these highest totals: PP-OCRv6 tiny detector 720
/// million elements, small 1.08 billion, medium 2.88 billion (about 2.3 billion of it the scratch of its widest 3 × 3
/// convolution, Conv.121, with its padded input, the layout copy and the output beside it). The recogniser (6 lines of
/// 48 × 3200) peaks at 180 million in the small and medium sets (66 million in tiny) and the orientation classifier (32
/// lines of 80 × 160) at 7.5 million. 4.5 billion is 1.56 times the largest of them, so every shipped set passes at the
/// largest photo with room to spare, and a model that keeps results or builds buffers past it is an error.
const MAX_LIVE_ELEMENTS: usize = 4_500_000_000;
/// Longest int64 vector (a shape, in practice a handful of values) Concat may build: the same vector can be named by
/// every input and fed back through node after node.
const MAX_SHAPE_LEN: usize = 1 << 16;
/// Largest dilation and padding a window may have (PP-OCR's use a dilation of 1 and pads of a few pixels): a model file
/// asking for more is refused at load, before any arithmetic on it.
const MAX_DILATION: usize = 64;
const MAX_PAD: usize = 1024;
/// Largest kernel side a window may have (PP-OCR's are a few taps): a model file asking for more is refused at load,
/// before a pad of half that size is allocated.
const MAX_KERNEL: usize = 1024;
/// Largest stride a window may have (PP-OCR's are 1 or 2): the depthwise path allocates one phase per stride step, and a
/// ceil-mode pool and a transposed convolution size their output by it.
const MAX_STRIDE: usize = 64;

/// The default-domain operator sets whose semantics for these operators [`Graph`] follows.
pub const OPSETS: std::ops::RangeInclusive<i64> = 7..=17;

#[derive(Clone, Debug)]
enum Value {
    F(Tensor),
    I(Vec<i64>),
}

/// Padding of a 2-D window: explicit (top, left, bottom, right) or ONNX's SAME_UPPER (worked out per input size).
#[derive(Clone, Copy, Debug)]
enum Pads {
    Fixed([usize; 4]),
    SameUpper,
}

#[derive(Clone, Copy, Debug)]
struct Window {
    kernel: [usize; 2],
    strides: [usize; 2],
    dilation: usize,
    pads: Pads,
}

#[derive(Clone, Debug)]
enum Op {
    Conv {
        window: Window,
        groups: usize,
    },
    ConvTranspose {
        stride: usize,
    },
    Relu,
    Sigmoid,
    HardSigmoid {
        alpha: f64,
        beta: f64,
    },
    Erf,
    Sqrt,
    Add,
    Sub,
    Mul,
    Div,
    /// `exponent`: the exponent when it is a constant (read once, at load).
    Pow {
        exponent: Option<f32>,
    },
    /// Dropout and Identity: the input, unchanged (inference).
    Pass,
    GlobalAveragePool,
    BatchNorm {
        eps: f64,
    },
    MaxPool {
        window: Window,
        ceil: bool,
    },
    AveragePool {
        window: Window,
        ceil: bool,
    },
    ReduceMean {
        axes: Vec<i64>,
        keep: bool,
    },
    /// Before opset 13 Softmax flattens the dimensions from `axis` on into one; from 13 it is over `axis` alone.
    Softmax {
        axis: i64,
        flatten: bool,
    },
    MatMul,
    Shape,
    Gather,
    Unsqueeze {
        axes: Vec<i64>,
    },
    Squeeze {
        axes: Vec<i64>,
    },
    /// Opset 10+: starts, ends, axes, steps as inputs. Opset 7: as attributes.
    Slice {
        attrs: Option<(Vec<i64>, Vec<i64>, Vec<i64>)>,
    },
    Concat {
        axis: i64,
    },
    Resize,
    Transpose {
        perm: Option<Vec<usize>>,
    },
    Reshape,
}

struct Node {
    name: String,
    op_type: String,
    op: Op,
    inputs: Vec<String>,
    output: String,
    /// Values no later node reads, dropped after this one.
    free: Vec<String>,
}

/// A loaded graph, ready to run.
pub struct Graph {
    nodes: Vec<Node>,
    consts: HashMap<String, Value>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    device: Device,
}

fn msg(m: impl Into<String>) -> Error {
    Error::Model(m.into())
}

fn unsupported(n: &NodeProto, what: impl std::fmt::Display) -> Error {
    msg(format!("node {} ({}): {what}", n.name, n.op_type))
}

fn attr_int(n: &NodeProto, name: &str, default: i64) -> Result<i64> {
    match n.attr(name) {
        None => Ok(default),
        Some(Attr::Int(v)) => Ok(*v),
        Some(_) => Err(unsupported(n, format!("attribute {name} is not an integer"))),
    }
}

fn attr_float(n: &NodeProto, name: &str, default: f32) -> Result<f32> {
    match n.attr(name) {
        None => Ok(default),
        Some(Attr::Float(v)) => Ok(*v),
        Some(_) => Err(unsupported(n, format!("attribute {name} is not a float"))),
    }
}

fn attr_ints(n: &NodeProto, name: &str) -> Result<Option<Vec<i64>>> {
    match n.attr(name) {
        None => Ok(None),
        Some(Attr::Ints(v)) => Ok(Some(v.clone())),
        Some(_) => Err(unsupported(n, format!("attribute {name} is not a list of integers"))),
    }
}

fn attr_str(n: &NodeProto, name: &str, default: &str) -> Result<String> {
    match n.attr(name) {
        None => Ok(default.to_string()),
        Some(Attr::Bytes(v)) => Ok(String::from_utf8_lossy(v).into_owned()),
        Some(_) => Err(unsupported(n, format!("attribute {name} is not a string"))),
    }
}

fn usizes(n: &NodeProto, name: &str, v: &[i64]) -> Result<Vec<usize>> {
    v.iter().map(|x| usize::try_from(*x).map_err(|_| unsupported(n, format!("negative {name} {x}")))).collect()
}

fn pair(n: &NodeProto, name: &str, default: usize) -> Result<[usize; 2]> {
    let v = usizes(n, name, &attr_ints(n, name)?.unwrap_or_else(|| vec![default as i64; 2]))?;
    match v[..] {
        [a, b] if a > 0 && b > 0 => Ok([a, b]),
        _ => Err(unsupported(n, format!("{name} {v:?} (needs two positive values)"))),
    }
}

/// A 2-D window's kernel, strides, dilation and padding. `kernel` comes from the weights for a convolution.
fn window(n: &NodeProto, kernel: Option<[usize; 2]>) -> Result<Window> {
    let kernel = match kernel {
        Some(k) => k,
        None => pair(n, "kernel_shape", 1)?,
    };
    if kernel.contains(&0) {
        return Err(unsupported(n, format!("an empty kernel {kernel:?}")));
    }
    if kernel.iter().any(|k| *k > MAX_KERNEL) {
        return Err(unsupported(n, format!("kernel {kernel:?} (at most {MAX_KERNEL})")));
    }
    let [dh, dw] = pair(n, "dilations", 1)?;
    if dh != dw {
        return Err(unsupported(n, "unequal dilations"));
    }
    if dh > MAX_DILATION {
        return Err(unsupported(n, format!("dilation {dh} (at most {MAX_DILATION})")));
    }
    let pads = match attr_str(n, "auto_pad", "NOTSET")?.as_str() {
        "NOTSET" => {
            let p = usizes(n, "pads", &attr_ints(n, "pads")?.unwrap_or_else(|| vec![0; 4]))?;
            let [t, l, b, r] = p[..] else { return Err(unsupported(n, "only 2-D windows are supported")) };
            if p.iter().any(|v| *v > MAX_PAD) {
                return Err(unsupported(n, format!("pads {p:?} (at most {MAX_PAD})")));
            }
            Pads::Fixed([t, l, b, r])
        }
        "SAME_UPPER" => Pads::SameUpper,
        "VALID" => Pads::Fixed([0; 4]),
        other => return Err(unsupported(n, format!("auto_pad {other}"))),
    };
    let strides = pair(n, "strides", 1)?;
    if strides.iter().any(|s| *s > MAX_STRIDE) {
        return Err(unsupported(n, format!("strides {strides:?} (at most {MAX_STRIDE})")));
    }
    Ok(Window { kernel, strides, dilation: dh, pads })
}

/// The extent of a kernel of `k` taps `dilation` apart: `(k - 1) · dilation + 1`, checked.
fn span(k: usize, dilation: usize) -> Result<usize> {
    k.checked_sub(1)
        .and_then(|k| k.checked_mul(dilation))
        .and_then(|e| e.checked_add(1))
        .ok_or_else(|| msg(format!("a window of {k} taps with dilation {dilation}")))
}

impl Window {
    /// (top, left, bottom, right) for an input of `h` × `w`.
    fn pads(&self, h: usize, w: usize) -> Result<[usize; 4]> {
        match self.pads {
            Pads::Fixed(p) => Ok(p),
            Pads::SameUpper => {
                let total = |len: usize, k: usize, s: usize| -> Result<usize> {
                    let out = len.div_ceil(s.max(1));
                    let span = span(k, self.dilation)?;
                    let reach = out.saturating_sub(1).checked_mul(s).and_then(|v| v.checked_add(span)).ok_or_else(|| msg("a window too large"))?;
                    Ok(reach.saturating_sub(len))
                };
                let ([kh, kw], [sh, sw]) = (self.kernel, self.strides);
                let (th, tw) = (total(h, kh, sh)?, total(w, kw, sw)?);
                Ok([th / 2, tw / 2, th - th / 2, tw - tw / 2])
            }
        }
    }
}

fn compile(n: &NodeProto, consts: &HashMap<String, Value>, opset: i64) -> Result<Op> {
    if !(n.domain.is_empty() || n.domain == "ai.onnx") {
        return Err(unsupported(n, format!("operator domain {:?} is not supported", n.domain)));
    }
    Ok(match n.op_type.as_str() {
        "Conv" => {
            let w = n.inputs.get(1).and_then(|w| consts.get(w));
            let kernel = match w {
                Some(Value::F(t)) if t.rank() == 4 => Some([t.dim(2)?, t.dim(3)?]),
                _ => return Err(unsupported(n, "the weights must be a constant 4-D tensor")),
            };
            let groups = usize::try_from(attr_int(n, "group", 1)?).ok().filter(|g| *g > 0).ok_or_else(|| unsupported(n, "bad group"))?;
            Op::Conv { window: window(n, kernel)?, groups }
        }
        "ConvTranspose" => {
            // the kernel is the weights' last two dims; candle's output-size arithmetic is unchecked, so an empty one is refused here
            let kernel = match n.inputs.get(1).and_then(|w| consts.get(w)) {
                Some(Value::F(t)) if t.rank() == 4 => [t.dim(2)?, t.dim(3)?],
                _ => return Err(unsupported(n, "the weights must be a constant 4-D tensor")),
            };
            let win = window(n, Some(kernel))?;
            let [sh, sw] = win.strides;
            if sh != sw || win.dilation != 1 || attr_int(n, "group", 1)? != 1 || !matches!(win.pads, Pads::Fixed([0, 0, 0, 0])) {
                return Err(unsupported(n, "only an unpadded, ungrouped transposed convolution with equal strides is supported"));
            }
            if attr_ints(n, "output_padding")?.is_some_and(|p| p.iter().any(|v| *v != 0)) || n.attr("output_shape").is_some() {
                return Err(unsupported(n, "output_padding / output_shape"));
            }
            Op::ConvTranspose { stride: sh }
        }
        "Relu" => Op::Relu,
        "Sigmoid" => Op::Sigmoid,
        "HardSigmoid" => Op::HardSigmoid { alpha: f64::from(attr_float(n, "alpha", 0.2)?), beta: f64::from(attr_float(n, "beta", 0.5)?) },
        "Erf" => Op::Erf,
        "Sqrt" => Op::Sqrt,
        "Add" => Op::Add,
        "Sub" => Op::Sub,
        "Mul" => Op::Mul,
        "Div" => Op::Div,
        "Pow" => Op::Pow {
            exponent: match n.inputs.get(1).and_then(|e| consts.get(e)) {
                Some(Value::F(t)) if t.elem_count() == 1 => t.flatten_all()?.to_vec1::<f32>()?.first().copied(),
                _ => None,
            },
        },
        "Identity" => Op::Pass,
        "Dropout" => {
            if attr_int(n, "training_mode", 0)? != 0 {
                return Err(unsupported(n, "training mode"));
            }
            Op::Pass
        }
        "GlobalAveragePool" => Op::GlobalAveragePool,
        "BatchNormalization" => {
            if attr_int(n, "training_mode", 0)? != 0 || attr_int(n, "spatial", 1)? != 1 {
                return Err(unsupported(n, "training mode or non-spatial"));
            }
            Op::BatchNorm { eps: f64::from(attr_float(n, "epsilon", 1e-5)?) }
        }
        "MaxPool" | "AveragePool" => {
            if attr_int(n, "storage_order", 0)? != 0 {
                return Err(unsupported(n, "storage_order"));
            }
            let win = window(n, None)?;
            let ceil = attr_int(n, "ceil_mode", 0)? != 0;
            if n.op_type == "MaxPool" {
                Op::MaxPool { window: win, ceil }
            } else {
                // padding that counts would change the averages at the borders
                if attr_int(n, "count_include_pad", 0)? != 0 && !matches!(win.pads, Pads::Fixed([0, 0, 0, 0])) {
                    return Err(unsupported(n, "count_include_pad with padding"));
                }
                if !matches!(win.pads, Pads::Fixed([0, 0, 0, 0])) {
                    return Err(unsupported(n, "padded average pooling"));
                }
                Op::AveragePool { window: win, ceil }
            }
        }
        "ReduceMean" => {
            if n.inputs.len() > 1 {
                return Err(unsupported(n, "axes as an input (opset 18) are not supported"));
            }
            Op::ReduceMean {
                axes: attr_ints(n, "axes")?.ok_or_else(|| unsupported(n, "reducing over every axis is not supported"))?,
                keep: attr_int(n, "keepdims", 1)? != 0,
            }
        }
        "Softmax" => Op::Softmax { axis: attr_int(n, "axis", if opset >= 13 { -1 } else { 1 })?, flatten: opset < 13 },
        "MatMul" => Op::MatMul,
        "Shape" => {
            // opset 15's start / end would slice the dims; the run always returns every one
            if n.attr("start").is_some() || n.attr("end").is_some() {
                return Err(unsupported(n, "Shape start / end (opset 15) are not supported"));
            }
            Op::Shape
        }
        "Gather" => {
            if attr_int(n, "axis", 0)? != 0 {
                return Err(unsupported(n, "Gather is only supported on axis 0 of a shape"));
            }
            Op::Gather
        }
        "Unsqueeze" => Op::Unsqueeze { axes: attr_ints(n, "axes")?.ok_or_else(|| unsupported(n, "axes as an input (opset 13) are not supported"))? },
        "Squeeze" => Op::Squeeze { axes: attr_ints(n, "axes")?.ok_or_else(|| unsupported(n, "Squeeze needs its axes as an attribute"))? },
        "Slice" => {
            if opset < 10 {
                let starts = attr_ints(n, "starts")?.ok_or_else(|| unsupported(n, "no starts"))?;
                let ends = attr_ints(n, "ends")?.ok_or_else(|| unsupported(n, "no ends"))?;
                let axes = attr_ints(n, "axes")?.unwrap_or_else(|| (0..starts.len() as i64).collect());
                Op::Slice { attrs: Some((starts, ends, axes)) }
            } else {
                Op::Slice { attrs: None }
            }
        }
        "Concat" => Op::Concat { axis: attr_int(n, "axis", i64::MIN).ok().filter(|a| *a != i64::MIN).ok_or_else(|| unsupported(n, "no axis"))? },
        "Resize" => {
            let mode = attr_str(n, "mode", "nearest")?;
            let ctm = attr_str(n, "coordinate_transformation_mode", "half_pixel")?;
            let nearest = attr_str(n, "nearest_mode", "round_prefer_floor")?;
            if (mode.as_str(), ctm.as_str(), nearest.as_str()) != ("nearest", "asymmetric", "floor") {
                return Err(unsupported(n, format!("Resize {mode}/{ctm}/{nearest} (only nearest/asymmetric/floor)")));
            }
            Op::Resize
        }
        "Transpose" => Op::Transpose { perm: attr_ints(n, "perm")?.map(|p| usizes(n, "perm", &p)).transpose()? },
        "Reshape" => {
            if attr_int(n, "allowzero", 0)? != 0 {
                return Err(unsupported(n, "allowzero"));
            }
            Op::Reshape
        }
        _ => return Err(unsupported(n, "operator not supported")),
    })
}

fn constant(t: TensorProto, device: &Device) -> Result<Value> {
    Ok(match t.data {
        Data::F32(v) => Value::F(Tensor::from_vec(v, t.dims, device)?),
        Data::I64(v) => Value::I(v),
    })
}

impl Graph {
    pub fn load(path: &Path, device: &Device) -> Result<Self> {
        let model = onnx::read(path)?;
        Self::new(model, device).map_err(|e| match e {
            Error::Model(m) => msg(format!("{}: {m}", path.display())),
            e => e,
        })
    }

    pub fn new(model: ModelProto, device: &Device) -> Result<Self> {
        let opset = match model.opset {
            Some(v) if OPSETS.contains(&v) => v,
            other => return Err(msg(format!("operator set {other:?} is not supported (needs {} to {})", OPSETS.start(), OPSETS.end()))),
        };
        let mut consts = HashMap::new();
        for t in model.initializers {
            let name = t.name.clone();
            consts.insert(name, constant(t, device)?);
        }
        let inputs: Vec<String> = model.inputs.into_iter().filter(|i| !consts.contains_key(i)).collect();
        // every value must be computed before it is used
        let mut known: HashSet<String> = consts.keys().cloned().chain(inputs.iter().cloned()).collect();
        let mut nodes = Vec::with_capacity(model.nodes.len());
        for n in &model.nodes {
            for i in &n.inputs {
                if !i.is_empty() && !known.contains(i) {
                    return Err(unsupported(n, format!("uses {i:?} before it is computed")));
                }
            }
            let output = n.outputs.first().filter(|o| !o.is_empty()).ok_or_else(|| unsupported(n, "no output"))?;
            if n.op_type == "Constant" {
                let Some(Attr::Tensor(t)) = n.attr("value") else { return Err(unsupported(n, "only a tensor `value` is supported")) };
                consts.insert(output.clone(), constant(t.clone(), device)?);
                known.insert(output.clone());
                continue;
            }
            // since opset 12 Dropout's training mode is its third input: only a constant false one is inference
            if n.op_type == "Dropout"
                && let Some(t) = n.inputs.get(2).filter(|t| !t.is_empty())
                && !matches!(consts.get(t.as_str()), Some(Value::I(v)) if v.iter().all(|x| *x == 0))
            {
                return Err(unsupported(n, "training mode"));
            }
            known.insert(output.clone());
            nodes.push(Node {
                name: n.name.clone(),
                op_type: n.op_type.clone(),
                op: compile(n, &consts, opset)?,
                inputs: n.inputs.clone(),
                output: output.clone(),
                free: Vec::new(),
            });
        }
        for o in &model.outputs {
            if !known.contains(o) {
                return Err(msg(format!("graph output {o:?} is never computed")));
            }
        }
        // drop each intermediate value after its last reader
        let mut last: HashMap<&str, usize> = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            for name in n.inputs.iter().filter(|s| !s.is_empty()) {
                last.insert(name.as_str(), i);
            }
        }
        let keep: HashSet<&str> = model.outputs.iter().map(String::as_str).chain(consts.keys().map(String::as_str)).collect();
        let free: Vec<(usize, String)> = last.into_iter().filter(|(n, _)| !keep.contains(n)).map(|(n, i)| (i, n.to_string())).collect();
        for (i, name) in free {
            if let Some(node) = nodes.get_mut(i) {
                node.free.push(name);
            }
        }
        Ok(Self { nodes, consts, inputs, outputs: model.outputs, device: device.clone() })
    }

    /// The device the graph's weights live on.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Runs the graph on its single input. The outputs, in the graph's order.
    pub fn run(&self, x: &Tensor) -> Result<Vec<Tensor>> {
        self.run_within(x, MAX_LIVE_ELEMENTS).map(|(outputs, _)| outputs)
    }

    /// [`Graph::run`] holding at most `limit` elements at once; also the most it held (see [`Budget`]).
    fn run_within(&self, x: &Tensor, limit: usize) -> Result<(Vec<Tensor>, usize)> {
        let [name] = &self.inputs[..] else { return Err(msg(format!("the model has {} inputs, not one", self.inputs.len()))) };
        let mut env: HashMap<&str, Value> = HashMap::new();
        let input = Value::F(x.to_device(&self.device)?);
        let budget = Budget::new(limit);
        budget.hold(held(&input))?;
        env.insert(name.as_str(), input);
        // LIGHTCRAFT_PROFILE=ops: the time of each operator type, waiting for the device after each node
        let profile = std::env::var("LIGHTCRAFT_PROFILE").is_ok_and(|v| v == "ops");
        let mut spent: HashMap<&str, (f64, usize)> = HashMap::new();
        for node in &self.nodes {
            let started = std::time::Instant::now();
            let out = {
                let mut args = Vec::with_capacity(node.inputs.len());
                for name in &node.inputs {
                    args.push(if name.is_empty() {
                        None
                    } else {
                        Some(env.get(name.as_str()).or_else(|| self.consts.get(name)).ok_or_else(|| msg(format!("value {name:?} is missing")))?)
                    });
                }
                exec(&budget, &node.op, &args).map_err(|e| node_error(node, e))?
            };
            if profile {
                self.device.synchronize()?;
                let e = spent.entry(node.op_type.as_str()).or_default();
                e.0 += started.elapsed().as_secs_f64() * 1000.0;
                e.1 += 1;
            }
            // the operator's reservations are gone: its result is now one of the run's values (an operator that only
            // returns a view of an input reserved nothing, and its result is checked here)
            budget.hold(held(&out)).map_err(|e| node_error(node, e))?;
            for f in &node.free {
                if let Some(v) = env.remove(f.as_str()) {
                    budget.release(held(&v));
                }
            }
            if let Some(old) = env.insert(node.output.as_str(), out) {
                budget.release(held(&old));
            }
        }
        if profile {
            let mut rows: Vec<_> = spent.into_iter().collect();
            rows.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
            let table: Vec<String> = rows.iter().take(8).map(|(op, (ms, n))| format!("{op} {ms:.1} ms ({n})")).collect();
            eprintln!("graph {:?} on {:?}: {}", x.dims(), self.device.location(), table.join(", "));
        }
        self.outputs
            .iter()
            .map(|o| match env.remove(o.as_str()).or_else(|| self.consts.get(o).cloned()) {
                Some(Value::F(t)) => Ok(t),
                Some(Value::I(_)) => Err(msg(format!("output {o:?} is not a float tensor"))),
                None => Err(msg(format!("output {o:?} was not computed"))),
            })
            .collect::<Result<Vec<_>>>()
            .map(|outputs| (outputs, budget.peak()))
    }
}

/// `e` raised by `node`, named.
fn node_error(node: &Node, e: Error) -> Error {
    let m = match e {
        Error::Model(m) | Error::Input(m) => m,
        other => other.to_string(),
    };
    msg(format!("node {} ({}): {m}", node.name, node.op_type))
}

fn arg<'a>(a: &[Option<&'a Value>], i: usize) -> Option<&'a Value> {
    a.get(i).copied().flatten()
}

fn tensor<'a>(a: &[Option<&'a Value>], i: usize) -> Result<&'a Tensor> {
    match arg(a, i) {
        Some(Value::F(t)) => Ok(t),
        Some(Value::I(_)) => Err(msg(format!("input {i} must be a float tensor"))),
        None => Err(msg(format!("input {i} is missing"))),
    }
}

fn ints<'a>(a: &[Option<&'a Value>], i: usize) -> Result<&'a [i64]> {
    match arg(a, i) {
        Some(Value::I(v)) => Ok(v),
        Some(Value::F(_)) => Err(msg(format!("input {i} must be an int64 tensor"))),
        None => Err(msg(format!("input {i} is missing"))),
    }
}

/// `t`'s dims with leading 1s up to rank `r` (numpy-style broadcasting aligns trailing dims).
fn lifted_dims(t: &Tensor, r: usize) -> Vec<usize> {
    let mut d = vec![1; r.saturating_sub(t.rank())];
    d.extend_from_slice(t.dims());
    d
}

/// `t` with leading 1s up to rank `r`.
fn lift(t: &Tensor, r: usize) -> Result<Tensor> {
    if t.rank() >= r {
        return Ok(t.clone());
    }
    Ok(t.reshape(lifted_dims(t, r))?)
}

/// The sum of `parts`, checked.
fn total(parts: &[usize]) -> Result<usize> {
    parts.iter().try_fold(0usize, |s, p| s.checked_add(*p)).ok_or_else(|| msg("a size too large"))
}

/// `n` times `k`, checked.
fn times(n: usize, k: usize) -> Result<usize> {
    n.checked_mul(k).ok_or_else(|| msg("a size too large"))
}

/// Both operands lifted to a common rank, with the broadcast result (the per-axis maximum, checked against the
/// per-tensor ceiling) and the copies lifting a non-contiguous operand makes reserved until the returned guard drops.
fn broadcast<'b>(budget: &'b Budget, a: &Tensor, b: &Tensor) -> Result<(Tensor, Tensor, Reservation<'b>)> {
    let r = a.rank().max(b.rank());
    let out: Vec<usize> = lifted_dims(a, r).iter().zip(lifted_dims(b, r)).map(|(x, y)| *x.max(&y)).collect();
    let result = checked_elements(&out)?;
    let copies = [a, b].into_iter().filter(|t| t.rank() < r && !t.is_contiguous()).map(Tensor::elem_count).sum::<usize>();
    let held = budget.reserve(total(&[result, copies])?)?;
    Ok((lift(a, r)?, lift(b, r)?, held))
}

/// The elements a matrix product holds: its output (batch dimensions broadcast, then `[m, n]`) and the operands candle
/// may expand to that batch. Every one of those shapes is checked against the per-tensor ceiling; nothing is reserved
/// here.
fn check_matmul(x: &Tensor, y: &Tensor) -> Result<usize> {
    let (Some((bx, mk)), Some((by, kn))) =
        (x.dims().split_at_checked(x.rank().saturating_sub(2)), y.dims().split_at_checked(y.rank().saturating_sub(2)))
    else {
        return Err(msg("MatMul of vectors is not supported"));
    };
    let (&[m, k], &[k2, n]) = (mk, kn) else { return Err(msg("MatMul of vectors is not supported")) };
    if k != k2 {
        return Err(msg(format!("can't multiply {:?} by {:?}", x.dims(), y.dims())));
    }
    let nb = bx.len().max(by.len());
    // batch dimensions align on the right, missing ones are 1
    let side = |b: &[usize], i: usize| (i + b.len()).checked_sub(nb).and_then(|j| b.get(j)).copied().unwrap_or(1);
    let batch: Vec<usize> = (0..nb).map(|i| side(bx, i).max(side(by, i))).collect();
    let mut sizes = [0usize; 3];
    for (size, rest) in sizes.iter_mut().zip([[m, n], [m, k], [k, n]]) {
        let mut dims = batch.clone();
        dims.extend(rest);
        *size = checked_elements(&dims)?;
    }
    let [result, lhs, rhs] = sizes;
    // an operand whose batch dimensions are not the broadcast ones is expanded to them
    let expanded = |b: &[usize], size: usize| if b == batch.as_slice() { 0 } else { size };
    total(&[result, expanded(bx, lhs), expanded(by, rhs)])
}

/// `y` plus a per-channel bias. A bias that is not one value per channel would broadcast the output larger. The sum is a
/// second buffer of `y`'s size while `y` is alive.
fn add_bias(budget: &Budget, y: Tensor, a: &[Option<&Value>]) -> Result<Tensor> {
    if arg(a, 2).is_none() {
        return Ok(y);
    }
    let bias = tensor(a, 2)?;
    let c = y.dim(1)?;
    if bias.elem_count() != c {
        return Err(msg(format!("a bias of {} values for {c} channels", bias.elem_count())));
    }
    let _sum = budget.reserve(y.elem_count())?;
    Ok(y.broadcast_add(&bias.reshape((1, c, 1, 1))?)?)
}

/// `axis` (negative counts from the end) as an index into `rank` dims.
fn axis(axis: i64, rank: usize) -> Result<usize> {
    let r = i64::try_from(rank).map_err(|_| msg("rank"))?;
    let a = if axis < 0 { axis.saturating_add(r) } else { axis };
    usize::try_from(a).ok().filter(|a| *a < rank).ok_or_else(|| msg(format!("axis {axis} out of range for rank {rank}")))
}

/// ONNX Slice bounds on a dimension of `len`: negatives count from the end, then clamped.
fn slice_range(start: i64, end: i64, len: usize) -> (usize, usize) {
    let n = i64::try_from(len).unwrap_or(i64::MAX);
    let fix = |v: i64| (if v < 0 { v.saturating_add(n) } else { v }).clamp(0, n);
    let (s, e) = (fix(start), fix(end));
    (s as usize, e.max(s) as usize)
}

/// `x` in a layout candle's CPU conv2d reads correctly. Its tiled im2col path assumes an input with strides
/// `[h·w·c, w·c, c, 1]` is stored channels-last; a contiguous NCHW tensor with c = h = w has exactly those strides and
/// would be misread. The same values with channels-last storage can't be mistaken. That copy of `x` is reserved until
/// the returned guard drops.
fn conv_safe_layout<'b>(budget: &'b Budget, x: &Tensor) -> Result<(Tensor, Option<Reservation<'b>>)> {
    let (_, c, h, w) = x.dims4()?;
    if x.device().is_cpu() && c > 1 && x.stride() == [h * w * c, w * c, c, 1] {
        let copy = budget.reserve(x.elem_count())?;
        return Ok((x.permute((0, 2, 3, 1))?.contiguous()?.permute((0, 3, 1, 2))?, Some(copy)));
    }
    Ok((x.clone(), None))
}

/// The elements of a tensor of `dims`, checked against [`MAX_RUNTIME_ELEMENTS`]. A shape check only: it reserves nothing
/// (see [`Budget::tensor`]).
fn checked_elements(dims: &[usize]) -> Result<usize> {
    checked_within(dims, MAX_RUNTIME_ELEMENTS)
}

/// [`checked_elements`] for a scratch buffer (an im2col or column buffer), against [`MAX_SCRATCH_ELEMENTS`].
fn checked_scratch(dims: &[usize]) -> Result<usize> {
    checked_within(dims, MAX_SCRATCH_ELEMENTS)
}

fn checked_within(dims: &[usize], limit: usize) -> Result<usize> {
    dims.iter().try_fold(1usize, |a, d| a.checked_mul(*d)).filter(|n| *n <= limit).ok_or_else(|| msg(format!("a tensor of {dims:?} is too large")))
}

/// Elements a value holds.
fn held(v: &Value) -> usize {
    match v {
        Value::F(t) => t.elem_count(),
        Value::I(v) => v.len(),
    }
}

/// The elements one run holds at once, against the most it may hold ([`MAX_LIVE_ELEMENTS`] in a real run). It is made by
/// [`Graph::run`] and passed to every operator that allocates, so nothing about it is global.
///
/// Two kinds of element are counted in one total, `held`:
/// - The run's values: the input and every operator result not yet freed, including the graph outputs, which are never
///   freed. [`Budget::hold`] adds a result when it is stored and [`Budget::release`] takes it off when it is freed. A
///   value shared with another (a Reshape of it) counts once per name, so the total is an upper bound.
/// - An operator's buffers: [`Budget::reserve`] adds the elements of a buffer the operator is about to allocate and
///   returns a [`Reservation`] that takes them off when it drops. An operator reserves every buffer that exists at the
///   same time (a padded copy of the input, the scratch, the output) before building them, and keeps the guards until
///   its result exists, so the total at any point is what the process really holds there, and an error at any `?`
///   releases them. The operator's own reservations are gone before the run stores its result with `hold`.
///
/// A request that would take `held` over the limit is an error naming it; `peak` is the highest `held` reached.
///
/// Not counted: constants and initializers (bounded by the model file's size, read at load); the vectors of
/// per-channel parameters and shape tuples, a few values per tensor dimension; and what candle allocates inside one
/// kernel beyond the buffers named at the call (a tiled convolution's per-thread tiles, a pooling kernel's window
/// state). A Softmax's three buffers, a mean's sum and quotient and a convolution's channels-last copy of the input are
/// reserved because they are tensors the call holds, not because they are candle's own.
struct Budget {
    limit: usize,
    held: Cell<usize>,
    peak: Cell<usize>,
}

/// Elements a [`Budget`] has lent to a buffer; given back when it drops.
struct Reservation<'a> {
    budget: &'a Budget,
    n: usize,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.budget.release(self.n);
    }
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self { limit, held: Cell::new(0), peak: Cell::new(0) }
    }

    /// The elements held now.
    #[cfg(test)]
    fn held(&self) -> usize {
        self.held.get()
    }

    /// The most elements held at any point.
    fn peak(&self) -> usize {
        self.peak.get()
    }

    /// Adds `n` elements, described by `what` if that would be over the limit.
    fn add(&self, n: usize, what: impl FnOnce() -> String) -> Result<()> {
        let held = self.held.get();
        let total = held.checked_add(n).filter(|t| *t <= self.limit).ok_or_else(|| {
            msg(format!("{} on top of the {held} elements the run already holds is over the {} it may hold at once", what(), self.limit))
        })?;
        self.held.set(total);
        self.peak.set(self.peak.get().max(total));
        Ok(())
    }

    /// Counts a result the run keeps until [`Budget::release`].
    fn hold(&self, n: usize) -> Result<()> {
        self.add(n, || format!("a result of {n} elements"))
    }

    fn release(&self, n: usize) {
        self.held.set(self.held.get().saturating_sub(n));
    }

    /// Lends `n` elements to a buffer about to be allocated.
    fn reserve(&self, n: usize) -> Result<Reservation<'_>> {
        self.add(n, || format!("{n} more elements"))?;
        Ok(Reservation { budget: self, n })
    }

    /// [`Budget::reserve`] for a tensor of `dims`, which is also checked against [`MAX_RUNTIME_ELEMENTS`].
    fn tensor(&self, dims: &[usize]) -> Result<Reservation<'_>> {
        let n = checked_elements(dims)?;
        self.add(n, || format!("a tensor of {dims:?}"))?;
        Ok(Reservation { budget: self, n })
    }

    /// [`Budget::reserve`] for a scratch buffer of `dims`, which is also checked against [`MAX_SCRATCH_ELEMENTS`].
    fn scratch(&self, dims: &[usize]) -> Result<Reservation<'_>> {
        let n = checked_scratch(dims)?;
        self.add(n, || format!("a scratch buffer of {dims:?}"))?;
        Ok(Reservation { budget: self, n })
    }
}

/// `x` padded by (top, left, bottom, right) with zeros, or with its edge values (`edge`). Reserves the padded copy, and
/// while it is built the copy with only the rows padded; the guard for the padded copy is returned.
fn pad2<'b>(budget: &'b Budget, x: &Tensor, [t, l, b, r]: [usize; 4], edge: bool) -> Result<(Tensor, Reservation<'b>)> {
    let (n, c, h, w) = x.dims4()?;
    let (padded_h, padded_w) = (total(&[h, t, b])?, total(&[w, l, r])?);
    let padded = budget.tensor(&[n, c, padded_h, padded_w])?;
    let pad = |x: &Tensor, dim: usize, before: usize, after: usize| -> Result<Tensor> {
        Ok(if edge { x.pad_with_same(dim, before, after)? } else { x.pad_with_zeros(dim, before, after)? })
    };
    // the rows-padded copy lives until the columns are padded too
    let (rows, _rows_guard) = if total(&[t, b])? > 0 {
        let guard = budget.tensor(&[n, c, padded_h, w])?;
        (pad(x, 2, t, b)?, Some(guard))
    } else {
        (x.clone(), None)
    };
    let out = if total(&[l, r])? > 0 { pad(&rows, 3, l, r)? } else { rows };
    Ok((out, padded))
}

/// Every `step`-th index of `dim` from 0 (a stride candle's ops can't take directly).
fn every(x: &Tensor, dim: usize, step: usize, count: usize) -> Result<Tensor> {
    if step == 1 && x.dim(dim)? == count {
        return Ok(x.clone());
    }
    let idx: Vec<u32> = (0..count).map(|i| (i * step) as u32).collect();
    Ok(x.index_select(&Tensor::from_vec(idx, count, x.device())?, dim)?)
}

/// Output length of a window over `len` (padded) values.
fn out_len(len: usize, k: usize, s: usize, ceil: bool) -> Result<usize> {
    if len < k {
        return Err(msg(format!("a feature map of {len} is smaller than the window of {k}")));
    }
    let span = len - k;
    Ok(if ceil { span.div_ceil(s) } else { span / s } + 1)
}

/// A pooling window over `x`. Holds the padded copy of `x` (and the ceil-mode extension of it) and the output at once.
fn pool(budget: &Budget, x: &Tensor, win: &Window, ceil: bool, max: bool) -> Result<Tensor> {
    let (n, c, h, w) = x.dims4()?;
    let [t, l, b, r] = win.pads(h, w)?;
    checked_elements(&[n, c, h.saturating_add(t).saturating_add(b), w.saturating_add(l).saturating_add(r)])?;
    // max pooling: padding with the edge values never wins over a real value, as ONNX's -inf padding
    let (x, _padded) = if total(&[t, l, b, r])? > 0 {
        let (padded, guard) = pad2(budget, x, [t, l, b, r], true)?;
        (padded, Some(guard))
    } else {
        (x.clone(), None)
    };
    let (_, _, h, w) = x.dims4()?;
    let ([kh, kw], [sh, sw]) = (win.kernel, win.strides);
    if win.dilation != 1 {
        return Err(msg("dilated pooling is not supported"));
    }
    let (oh, ow) = (out_len(h, kh, sh, ceil)?, out_len(w, kw, sw, ceil)?);
    // ceil mode: the last window may run past the edge; extend with edge values (max) — averages are refused there
    let reach = |o: usize, s: usize, k: usize| o.saturating_sub(1).checked_mul(s).and_then(|v| v.checked_add(k));
    let (Some(need_h), Some(need_w)) = (reach(oh, sh, kh), reach(ow, sw, kw)) else { return Err(msg("a pooling window too large")) };
    let (x, _extended) = if need_h > h || need_w > w {
        if !max {
            return Err(msg("ceil-mode average pooling over a partial window is not supported"));
        }
        // the pad is sized by the stride: reserved (and checked) before it is allocated
        let (extended, guard) = pad2(budget, &x, [0, 0, need_h.saturating_sub(h), need_w.saturating_sub(w)], true)?;
        (extended, Some(guard))
    } else {
        (x, None)
    };
    let _out = budget.tensor(&[n, c, oh, ow])?;
    // stride 1 windows, then every stride-th one (candle's pooling takes one stride per axis but equal kernels)
    let y = if max { x.max_pool2d_with_stride((kh, kw), (sh, sw))? } else { x.avg_pool2d_with_stride((kh, kw), (sh, sw))? };
    Ok(y.narrow(2, 0, oh.min(y.dim(2)?))?.narrow(3, 0, ow.min(y.dim(3)?))?)
}

/// A depthwise convolution (one filter per channel) as the sum of its kernel's taps: each tap is the input shifted by
/// the tap's offset times that tap's per-channel weight. candle runs a grouped convolution as one convolution per group
/// (hundreds of small kernels for a depthwise layer, very slow on a graphics card); this is `kh × kw` whole-tensor
/// operations instead.
///
/// The caller has reserved the output. This reserves what else exists with it: the padded input, each phase (and the copies
/// made while one is built), and the two other buffers of the running sum (the sum so far, the tap's term and their new
/// sum exist together).
fn depthwise(budget: &Budget, x: &Tensor, w: &Tensor, pad: usize, [sh, sw]: [usize; 2], dilation: usize) -> Result<Tensor> {
    let (x, _padded) = if pad > 0 {
        let (padded, guard) = pad2(budget, x, [pad; 4], false)?;
        (padded, Some(guard))
    } else {
        (x.clone(), None)
    };
    let (n, c, h, wd) = x.dims4()?;
    let (_, _, kh, kw) = w.dims4()?;
    if kh == 0 || kw == 0 || dilation == 0 {
        return Err(msg("an empty kernel"));
    }
    let (eh, ew) = ((kh - 1) * dilation + 1, (kw - 1) * dilation + 1);
    if h < eh || wd < ew || sh == 0 || sw == 0 {
        return Err(msg(format!("a feature map of {h}×{wd} is smaller than the window of {eh}×{ew}")));
    }
    let (oh, ow) = ((h - eh) / sh + 1, (wd - ew) / sw + 1);
    let _terms = if kh * kw > 1 { Some(budget.reserve(times(checked_elements(&[n, c, oh, ow])?, 2)?)?) } else { None };
    let taps = w.reshape((c, kh * kw))?.t()?.contiguous()?;
    // with a stride, the input's sh × sw phases (every sh-th row from row pi, every sw-th column from column pj), each
    // made once: a tap at offset (oi, oj) reads phase (oi % sh, oj % sw) from (oi / sh, oj / sw)
    let mut phases: Vec<Option<Tensor>> = vec![None; sh * sw];
    let mut phase_guards = Vec::new();
    let mut acc: Option<Tensor> = None;
    for i in 0..kh {
        for j in 0..kw {
            let (oi, oj) = (i * dilation, j * dilation);
            let (pi, pj) = (oi % sh, oj % sw);
            let slot = phases.get_mut(pi * sw + pj).ok_or_else(|| msg("phase"))?;
            let phase = match slot {
                Some(p) => p.clone(),
                None => {
                    let (rows, cols) = ((h - pi).div_ceil(sh), (wd - pj).div_ceil(sw));
                    // with a stride the phase is a copy that stays; the shifted copy it is picked from and the rows picked
                    // first exist while it is built (without one the phase is a view of the input)
                    let _building = if sh * sw > 1 {
                        phase_guards.push(budget.tensor(&[n, c, rows, cols])?);
                        Some((budget.tensor(&[n, c, h - pi, wd - pj])?, budget.tensor(&[n, c, rows, wd - pj])?))
                    } else {
                        None
                    };
                    let v = x.narrow(2, pi, h - pi)?.narrow(3, pj, wd - pj)?.contiguous()?;
                    let p = every(&every(&v, 2, sh, rows)?, 3, sw, cols)?;
                    slot.insert(p).clone()
                }
            };
            let xs = phase.narrow(2, oi / sh, oh)?.narrow(3, oj / sw, ow)?;
            let tap = taps.get(i * kw + j)?.reshape((1, c, 1, 1))?;
            let term = xs.broadcast_mul(&tap)?;
            acc = Some(match acc {
                Some(a) => (a + term)?,
                None => term,
            });
        }
    }
    acc.ok_or_else(|| msg("an empty kernel"))
}

/// An element-wise operator over the first input: `buffers` tensors of its size are reserved before `f` builds them.
fn unary(budget: &Budget, a: &[Option<&Value>], buffers: usize, f: impl FnOnce(&Tensor) -> candle_core::Result<Tensor>) -> Result<Tensor> {
    let x = tensor(a, 0)?;
    let _held = budget.reserve(times(x.elem_count(), buffers)?)?;
    Ok(f(x)?)
}

/// Runs one operator. Every buffer it holds at once is reserved in `budget` before it is built and stays reserved until
/// the result exists; an operator that only returns a view of an input reserves nothing.
fn exec(budget: &Budget, op: &Op, a: &[Option<&Value>]) -> Result<Value> {
    let out = match op {
        Op::Conv { window: win, groups } => {
            let (x, w) = (tensor(a, 0)?, tensor(a, 1)?);
            if x.rank() != 4 || w.rank() != 4 {
                return Err(msg("only 2-D convolutions are supported"));
            }
            let (batch, chans, h, wd) = x.dims4()?;
            let [t, l, b, r] = win.pads(h, wd)?;
            let (c_out, c_in_k, kh, kw) = w.dims4()?;
            // the padded input must hold the window (candle's output-size arithmetic is not checked)
            let (eh, ew) = (span(kh, win.dilation)?, span(kw, win.dilation)?);
            let (ph, pw) = (h.saturating_add(t).saturating_add(b), wd.saturating_add(l).saturating_add(r));
            if ph < eh || pw < ew {
                return Err(msg(format!("a feature map of {ph}×{pw} is smaller than the window of {eh}×{ew}")));
            }
            checked_elements(&[batch, chans, ph, pw])?;
            let [sh, sw] = win.strides;
            let uniform = t == l && l == b && b == r;
            let p = if uniform { t } else { 0 };
            let depthwise_path = *groups > 1 && c_in_k == 1 && c_out == *groups && chans == *groups;
            let pointwise_path = *groups == 1 && (kh, kw, sh, sw, p) == (1, 1, 1, 1, 0);
            // the output has the weights' channels, so it can be far larger than the input; unequal strides first build
            // the stride-1 output; the general path builds an im2col buffer of one kernel's taps per output position
            let (oh, ow) = (ph.saturating_sub(eh) / sh + 1, pw.saturating_sub(ew) / sw + 1);
            let (ah, aw) = if sh == sw || depthwise_path { (oh, ow) } else { (ph.saturating_sub(eh) + 1, pw.saturating_sub(ew) + 1) };
            let out = checked_elements(&[batch, c_out, ah, aw])?;
            let general = !depthwise_path && !pointwise_path;
            if general {
                checked_scratch(&[batch, ah, aw, c_in_k, kh, kw])?;
            }
            // Every buffer the convolution holds at once is reserved before it is built and kept until its result exists:
            // the padded input when the pads are not equal (equal ones are made inside the convolution), the output (for
            // unequal strides the stride-1 output, then the rows and the result picked from it), and below, per path,
            // what that path needs besides.
            let (x, _padded) = if uniform {
                (x.clone(), None)
            } else {
                let (padded, guard) = pad2(budget, x, [t, l, b, r], false)?;
                (padded, Some(guard))
            };
            let _out = budget.reserve(out)?;
            let _picked = if (ah, aw) == (oh, ow) {
                None
            } else {
                Some(budget.reserve(total(&[checked_elements(&[batch, c_out, oh, aw])?, checked_elements(&[batch, c_out, oh, ow])?])?)?)
            };
            let y = if depthwise_path {
                depthwise(budget, &x, w, p, [sh, sw], win.dilation)?
            } else if pointwise_path {
                // a pointwise convolution is one matrix product ([c_out, c_in] × [c_in, h·w] per image), not im2col
                let (n, c, h, wd) = x.dims4()?;
                let _contiguous = if x.is_contiguous() { None } else { Some(budget.reserve(x.elem_count())?) };
                let y = w.reshape((c_out, c))?.broadcast_matmul(&x.reshape((n, c, h * wd))?)?;
                y.reshape((n, c_out, h, wd))?
            } else {
                // candle lays the input out channels-last (a copy), builds the scratch, and runs a grouped convolution
                // group by group, concatenating the results (a second buffer of the output's size). The scratch is the
                // whole im2col buffer: the most any backend builds (the CPU's tiled kernel builds less).
                let _layout = budget.reserve(x.elem_count())?;
                let _scratch = budget.scratch(&[batch, ah, aw, c_in_k, kh, kw])?;
                let _concat = if *groups > 1 { Some(budget.reserve(out)?) } else { None };
                let (xl, _safe) = conv_safe_layout(budget, &x)?;
                if sh == sw {
                    xl.conv2d(w, p, sh, win.dilation, *groups)?
                } else {
                    // unequal strides: the stride-1 convolution, then every sh-th row and sw-th column
                    let y = xl.conv2d(w, p, 1, win.dilation, *groups)?;
                    let (_, _, yh, yw) = y.dims4()?;
                    let y = every(&y, 2, sh, (yh - 1) / sh + 1)?;
                    every(&y, 3, sw, (yw - 1) / sw + 1)?
                }
            };
            add_bias(budget, y, a)?
        }
        Op::ConvTranspose { stride } => {
            let (x, w) = (tensor(a, 0)?, tensor(a, 1)?);
            let (n, c_in, h, wd) = x.dims4()?;
            let (w_in, c_out, kh, kw) = w.dims4()?;
            if w_in != c_in {
                return Err(msg(format!("{c_in} input channels for weights of {:?}", w.dims())));
            }
            if h == 0 || wd == 0 || kh == 0 || kw == 0 {
                return Err(msg("a transposed convolution over an empty feature map or kernel"));
            }
            // the output grows with the stride and the weights' kernel; candle also builds a [c_out·kh·kw, h·w] product
            let grown = |len: usize, k: usize| {
                len.saturating_sub(1).checked_mul(*stride).and_then(|v| v.checked_add(k)).ok_or_else(|| msg("a transposed convolution too large"))
            };
            let out = checked_elements(&[n, c_out, grown(h, kh)?, grown(wd, kw)?])?;
            // the output, the column buffer, and the input laid out for the product (a copy)
            let _out = budget.reserve(out)?;
            let _scratch = budget.scratch(&[n, c_out, kh, kw, h, wd])?;
            let _layout = budget.reserve(x.elem_count())?;
            let y = x.contiguous()?.conv_transpose2d(&w.contiguous()?, 0, 0, *stride, 1)?;
            add_bias(budget, y, a)?
        }
        Op::Relu => unary(budget, a, 1, Tensor::relu)?,
        Op::Sigmoid => unary(budget, a, 1, candle_nn::ops::sigmoid)?,
        // the scaled values and the clamped ones exist together
        Op::HardSigmoid { alpha, beta } => unary(budget, a, 2, |x| x.affine(*alpha, *beta)?.clamp(0f32, 1f32))?,
        Op::Erf => unary(budget, a, 1, Tensor::erf)?,
        Op::Sqrt => unary(budget, a, 1, Tensor::sqrt)?,
        Op::Add => {
            let (x, y, _held) = broadcast(budget, tensor(a, 0)?, tensor(a, 1)?)?;
            x.broadcast_add(&y)?
        }
        Op::Sub => {
            let (x, y, _held) = broadcast(budget, tensor(a, 0)?, tensor(a, 1)?)?;
            x.broadcast_sub(&y)?
        }
        Op::Mul => {
            let (x, y, _held) = broadcast(budget, tensor(a, 0)?, tensor(a, 1)?)?;
            x.broadcast_mul(&y)?
        }
        Op::Div => {
            let (x, y, _held) = broadcast(budget, tensor(a, 0)?, tensor(a, 1)?)?;
            x.broadcast_div(&y)?
        }
        Op::Pow { exponent } => {
            let (x, e) = (tensor(a, 0)?, tensor(a, 1)?);
            // the graphs square (layer norm's variance); a general power goes through exp/log. A constant exponent was
            // read at load (no copy from the device per run).
            let scalar = match exponent {
                Some(p) => vec![*p],
                None => e.flatten_all()?.to_vec1::<f32>()?,
            };
            if let [p] = scalar[..] {
                let _out = budget.reserve(x.elem_count())?;
                match p {
                    2.0 => x.sqr()?,
                    1.0 => x.clone(),
                    0.5 => x.sqrt()?,
                    p => x.powf(f64::from(p))?,
                }
            } else {
                let (x, e, _held) = broadcast(budget, x, e)?;
                x.broadcast_pow(&e)?
            }
        }
        Op::Pass => match arg(a, 0) {
            Some(Value::I(v)) => return Ok(Value::I(v.clone())),
            _ => tensor(a, 0)?.clone(),
        },
        Op::GlobalAveragePool => {
            let x = tensor(a, 0)?;
            if x.rank() != 4 {
                return Err(msg(format!("GlobalAveragePool takes [N, C, H, W], got {:?}", x.dims())));
            }
            // each mean is a sum and a scaling: two buffers of the reduced size, the first over the columns, the second
            // over the rows
            let (n, c, h) = (x.dim(0)?, x.dim(1)?, x.dim(2)?);
            let _held = budget.reserve(times(total(&[checked_elements(&[n, c, h])?, checked_elements(&[n, c])?])?, 2)?)?;
            x.mean_keepdim(3)?.mean_keepdim(2)?
        }
        Op::BatchNorm { eps } => {
            let x = tensor(a, 0)?;
            let (scale, bias, mean, var) = (tensor(a, 1)?, tensor(a, 2)?, tensor(a, 3)?, tensor(a, 4)?);
            // one value per channel, as a vector: any other shape would broadcast the output (or the arithmetic on the
            // parameters) to the parameters' size
            let c = *x.dims().get(1).ok_or_else(|| msg("BatchNormalization needs a channel dimension"))?;
            if let Some(p) = [scale, bias, mean, var].into_iter().find(|p| p.dims() != [c]) {
                return Err(msg(format!("BatchNormalization parameters of shape {:?} for {c} channels", p.dims())));
            }
            // the scaled values and the shifted ones exist together
            let _held = budget.reserve(times(x.elem_count(), 2)?)?;
            let mul = (scale / (var + *eps)?.sqrt()?)?;
            let add = (bias - mean.mul(&mul)?)?;
            let mut d = vec![1; x.rank()];
            *d.get_mut(1).ok_or_else(|| msg("BatchNormalization needs a channel dimension"))? = mul.elem_count();
            x.broadcast_mul(&mul.reshape(d.clone())?)?.broadcast_add(&add.reshape(d)?)?
        }
        Op::MaxPool { window: win, ceil } => pool(budget, tensor(a, 0)?, win, *ceil, true)?,
        Op::AveragePool { window: win, ceil } => pool(budget, tensor(a, 0)?, win, *ceil, false)?,
        Op::ReduceMean { axes, keep } => {
            let x = tensor(a, 0)?;
            let rank = x.rank();
            let mut ax = axes.iter().map(|v| axis(*v, rank)).collect::<Result<Vec<_>>>()?;
            ax.sort_unstable();
            ax.dedup();
            let mut y = x.clone();
            // each mean is a sum and a scaling (two buffers of the reduced size), taken while the previous one is alive
            let mut held = Vec::new();
            for d in ax.iter().rev() {
                let reduced = y.elem_count() / y.dim(*d)?.max(1);
                let next = budget.reserve(times(reduced, 2)?)?;
                y = if *keep { y.mean_keepdim(*d)? } else { y.mean(*d)? };
                held.clear();
                held.push(next);
            }
            y
        }
        Op::Softmax { axis: ax, flatten } => {
            let x = tensor(a, 0)?;
            let d = axis(*ax, x.rank())?;
            // up to three buffers of x's size exist together: a copy made contiguous, the shifted values and the
            // exponentials of the composed softmax, the result
            let _held = budget.reserve(times(x.elem_count(), 3)?)?;
            if *flatten && d + 1 != x.rank() {
                let dims = x.dims().to_vec();
                let (lo, hi) = dims.split_at_checked(d).ok_or_else(|| msg("axis out of range"))?;
                let y = x.reshape((lo.iter().product::<usize>(), hi.iter().product::<usize>()))?;
                candle_nn::ops::softmax_last_dim(&y.contiguous()?)?.reshape(dims)?
            } else if d + 1 == x.rank() {
                candle_nn::ops::softmax_last_dim(&x.contiguous()?)?
            } else {
                candle_nn::ops::softmax(x, d)?
            }
        }
        Op::MatMul => {
            let (x, y) = (tensor(a, 0)?, tensor(a, 1)?);
            if x.rank() < 2 || y.rank() < 2 {
                return Err(msg("MatMul of vectors is not supported"));
            }
            // the output, the operands expanded to the batch and the contiguous copies of operands that are not
            let expected = check_matmul(x, y)?;
            let copies = [x, y].into_iter().filter(|t| !t.is_contiguous()).map(Tensor::elem_count).sum::<usize>();
            let _held = budget.reserve(total(&[expected, copies])?)?;
            x.contiguous()?.broadcast_matmul(&y.contiguous()?)?
        }
        Op::Shape => {
            return Ok(Value::I(match arg(a, 0) {
                Some(Value::F(t)) => t.dims().iter().map(|d| *d as i64).collect(),
                Some(Value::I(v)) => vec![v.len() as i64],
                None => return Err(msg("input 0 is missing")),
            }));
        }
        Op::Gather => {
            let (data, idx) = (ints(a, 0)?, ints(a, 1)?);
            let n = data.len() as i64;
            let v = idx
                .iter()
                .map(|i| {
                    let j = if *i < 0 { i.saturating_add(n) } else { *i };
                    usize::try_from(j).ok().and_then(|j| data.get(j)).copied().ok_or_else(|| msg(format!("index {i} out of range")))
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(Value::I(v));
        }
        Op::Unsqueeze { axes } => match arg(a, 0) {
            Some(Value::I(v)) => return Ok(Value::I(v.clone())),
            _ => {
                let mut t = tensor(a, 0)?.clone();
                let rank = t.rank().saturating_add(axes.len());
                let mut ax = axes.iter().map(|x| axis(*x, rank)).collect::<Result<Vec<_>>>()?;
                ax.sort_unstable();
                for x in ax {
                    t = t.unsqueeze(x)?;
                }
                t
            }
        },
        Op::Squeeze { axes } => match arg(a, 0) {
            Some(Value::I(v)) => return Ok(Value::I(v.clone())),
            _ => {
                let mut t = tensor(a, 0)?.clone();
                let mut ax = axes.iter().map(|x| axis(*x, t.rank())).collect::<Result<Vec<_>>>()?;
                ax.sort_unstable();
                ax.dedup();
                for x in ax.into_iter().rev() {
                    if t.dim(x)? != 1 {
                        return Err(msg(format!("can't squeeze axis {x} of {:?}", t.dims())));
                    }
                    t = t.squeeze(x)?;
                }
                t
            }
        },
        Op::Slice { attrs } => {
            let (starts, ends, axes): (Vec<i64>, Vec<i64>, Vec<i64>) = match attrs {
                Some((s, e, ax)) => (s.clone(), e.clone(), ax.clone()),
                None => {
                    let (s, e) = (ints(a, 1)?.to_vec(), ints(a, 2)?.to_vec());
                    let ax = match arg(a, 3) {
                        Some(_) => ints(a, 3)?.to_vec(),
                        None => (0..s.len() as i64).collect(),
                    };
                    if arg(a, 4).is_some() && ints(a, 4)?.iter().any(|s| *s != 1) {
                        return Err(msg("only step 1 slices are supported"));
                    }
                    (s, e, ax)
                }
            };
            if starts.len() != ends.len() || starts.len() != axes.len() {
                return Err(msg("starts, ends and axes differ in length"));
            }
            match arg(a, 0) {
                Some(Value::I(v)) => {
                    let (&[s], &[e], &[ax]) = (&starts[..], &ends[..], &axes[..]) else {
                        return Err(msg("slicing a shape takes one axis"));
                    };
                    axis(ax, 1)?;
                    let (s, e) = slice_range(s, e, v.len());
                    return Ok(Value::I(v.get(s..e).unwrap_or_default().to_vec()));
                }
                _ => {
                    let mut t = tensor(a, 0)?.clone();
                    for ((s, e), ax) in starts.iter().zip(&ends).zip(&axes) {
                        let ax = axis(*ax, t.rank())?;
                        let (s, e) = slice_range(*s, *e, t.dim(ax)?);
                        t = t.narrow(ax, s, e - s)?;
                    }
                    t
                }
            }
        }
        Op::Concat { axis: ax } => {
            if a.iter().all(|v| matches!(v, Some(Value::I(_)))) {
                let mut out = Vec::new();
                for i in 0..a.len() {
                    let part = ints(a, i)?;
                    if out.len().saturating_add(part.len()) > MAX_SHAPE_LEN {
                        return Err(msg(format!("a concatenated vector of more than {MAX_SHAPE_LEN} values is too large")));
                    }
                    out.extend_from_slice(part);
                }
                return Ok(Value::I(out));
            }
            let ts = (0..a.len()).map(|i| tensor(a, i)).collect::<Result<Vec<_>>>()?;
            let rank = ts.first().map_or(0, |t| t.rank());
            // the same value may be named by every input
            let total = ts.iter().try_fold(0usize, |s, t| s.checked_add(t.elem_count())).ok_or_else(|| msg("a concatenation too large"))?;
            let _out = budget.tensor(&[total])?;
            Tensor::cat(&ts, axis(*ax, rank)?)?
        }
        Op::Resize => {
            let x = tensor(a, 0)?;
            let (n, c, h, w) = x.dims4()?;
            if h == 0 || w == 0 {
                return Err(msg("empty input"));
            }
            let sizes = match arg(a, 3) {
                Some(Value::I(v)) if !v.is_empty() => Some(v.as_slice()),
                Some(Value::F(t)) if t.elem_count() > 0 => return Err(msg("sizes must be int64")),
                _ => None,
            };
            let (oh, ow, sh, sw) = match sizes {
                Some(&[sn, sc, oh, ow]) => {
                    let (oh, ow) = (usize::try_from(oh).map_err(|_| msg("negative size"))?, usize::try_from(ow).map_err(|_| msg("negative size"))?);
                    if sn != n as i64 || sc != c as i64 || oh == 0 || ow == 0 {
                        return Err(msg(format!("only spatial resizing is supported (sizes {sizes:?})")));
                    }
                    (oh, ow, oh as f32 / h as f32, ow as f32 / w as f32)
                }
                Some(s) => return Err(msg(format!("sizes {s:?} must have 4 values"))),
                None => {
                    let s = tensor(a, 2)?.flatten_all()?.to_vec1::<f32>()?;
                    let &[s0, s1, sh, sw] = &s[..] else { return Err(msg("scales must have 4 values")) };
                    if s0 != 1.0 || s1 != 1.0 || !(sh > 0.0 && sw > 0.0 && sh.is_finite() && sw.is_finite()) {
                        return Err(msg(format!("unsupported scales {s:?}")));
                    }
                    let (oh, ow) = ((h as f32 * sh).floor(), (w as f32 * sw).floor());
                    if !(oh >= 1.0 && ow >= 1.0 && oh < 1e9 && ow < 1e9) {
                        return Err(msg(format!("unsupported scales {s:?}")));
                    }
                    (oh as usize, ow as usize, sh, sw)
                }
            };
            // each side alone too (an input with no elements makes every product 0), and the intermediate, which has the
            // new rows and the old columns
            let (rows, cols) = (checked_elements(&[oh])?, checked_elements(&[ow])?);
            let (picked, result) = (checked_elements(&[n, c, oh, w])?, checked_elements(&[n, c, oh, ow])?);
            // the index vectors, the rows picked, the result, and a contiguous copy of the input when it is not
            let copy = if x.is_contiguous() { 0 } else { x.elem_count() };
            let _held = budget.reserve(total(&[rows, cols, picked, result, copy])?)?;
            let index = |out: usize, scale: f32, len: usize| -> Result<Tensor> {
                let v: Vec<u32> = (0..out).map(|i| ((i as f32 / scale).floor().max(0.0) as usize).min(len - 1) as u32).collect();
                Ok(Tensor::from_vec(v, out, x.device())?)
            };
            x.index_select(&index(oh, sh, h)?, 2)?.index_select(&index(ow, sw, w)?, 3)?
        }
        Op::Transpose { perm } => {
            let t = tensor(a, 0)?;
            let perm = perm.clone().unwrap_or_else(|| (0..t.rank()).rev().collect());
            if perm.len() != t.rank() {
                return Err(msg(format!("perm {perm:?} does not match rank {}", t.rank())));
            }
            t.permute(perm)?
        }
        Op::Reshape => {
            let (t, shape) = (tensor(a, 0)?, ints(a, 1)?);
            let mut out = Vec::with_capacity(shape.len());
            let mut infer = None;
            for (i, s) in shape.iter().enumerate() {
                match *s {
                    0 => out.push(*t.dims().get(i).ok_or_else(|| msg("0 in a shape beyond the input's rank"))?),
                    -1 if infer.is_none() => {
                        infer = Some(i);
                        out.push(1);
                    }
                    s if s > 0 => out.push(usize::try_from(s).map_err(|_| msg("bad shape"))?),
                    _ => return Err(msg(format!("bad shape {shape:?}"))),
                }
            }
            let known = checked_elements(&out)?;
            if let Some(i) = infer {
                if known == 0 || t.elem_count() % known != 0 {
                    return Err(msg(format!("can't reshape {:?} to {shape:?}", t.dims())));
                }
                if let Some(d) = out.get_mut(i) {
                    *d = t.elem_count() / known;
                }
            }
            // a copy made contiguous is the storage of the result
            let _copy = if t.is_contiguous() { None } else { Some(budget.reserve(t.elem_count())?) };
            t.contiguous()?.reshape(out)?
        }
    };
    Ok(Value::F(out))
}

#[cfg(test)]
mod tests {
    use candle_core::{Device, Tensor};

    use super::{Budget, Graph, Op, Result, Value, checked_elements, checked_scratch, exec};
    use crate::onnx::{self, encode::*};

    fn graph(bytes: &[u8]) -> Result<Graph> {
        Graph::new(onnx::parse(bytes)?, &Device::Cpu)
    }

    fn run1(g: &Graph, x: Tensor) -> Vec<f32> {
        g.run(&x).unwrap()[0].flatten_all().unwrap().to_vec1().unwrap()
    }

    #[test]
    fn same_upper_pads_at_the_end() {
        // a 2×2 max pool, stride 1, SAME_UPPER: the output keeps the input's size, the last row/column see the edge
        let m = model(
            &[node(
                "MaxPool",
                &["x"],
                &["y"],
                &[attr_ints("kernel_shape", &[2, 2]), attr_ints("strides", &[1, 1]), attr_str("auto_pad", "SAME_UPPER")],
            )],
            &[],
            &["x"],
            &["y"],
        );
        let g = graph(&m).unwrap();
        let x = Tensor::from_vec(vec![1f32, 5., 3., 2.], (1, 1, 2, 2), &Device::Cpu).unwrap();
        assert_eq!(run1(&g, x), vec![5., 5., 3., 2.]);
    }

    #[test]
    fn depthwise_taps_match_candles_grouped_convolution() {
        // 4 channels, 9×7 input: kernels 3×3 and 5×5, strides 1 and 2, padding, a dilation; against candle's own
        // per-group convolution
        let x: Vec<f32> = (0..4 * 9 * 7).map(|v| ((v * 37 % 23) as f32 - 11.0) / 7.0).collect();
        let x = Tensor::from_vec(x, (1, 4, 9, 7), &Device::Cpu).unwrap();
        for (k, stride, pad, dil) in [(3, 1, 1, 1), (3, 2, 1, 1), (5, 2, 2, 1), (5, 1, 2, 1), (3, 1, 2, 2)] {
            let w: Vec<f32> = (0..4 * k * k).map(|v| ((v * 13 % 17) as f32 - 8.0) / 9.0).collect();
            let w = Tensor::from_vec(w, (4, 1, k, k), &Device::Cpu).unwrap();
            let want = x.conv2d(&w, pad, stride, dil, 4).unwrap();
            let got = super::depthwise(&Budget::new(usize::MAX), &x, &w, pad, [stride, stride], dil).unwrap();
            assert_eq!(got.dims(), want.dims(), "k {k} stride {stride}");
            let diff = (got - want).unwrap().abs().unwrap().flatten_all().unwrap().max(0).unwrap().to_scalar::<f32>().unwrap();
            assert!(diff < 1e-4, "k {k} stride {stride} pad {pad} dilation {dil}: {diff}");
        }
        // a window larger than the map is an error, not a panic
        let w = Tensor::ones((4, 1, 3, 3), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(super::depthwise(&Budget::new(usize::MAX), &x.narrow(2, 0, 2).unwrap(), &w, 0, [1, 1], 1).is_err());
    }

    #[test]
    fn pointwise_convolution_is_a_matrix_product() {
        // 3 → 5 channels, 1×1, with bias: against candle's conv2d
        let xs: Vec<f32> = (0..2 * 3 * 4 * 6).map(|v| ((v * 31 % 19) as f32 - 9.0) / 5.0).collect();
        let ws: Vec<f32> = (0..5 * 3).map(|v| ((v * 7 % 11) as f32 - 5.0) / 4.0).collect();
        let m = model(
            &[node("Conv", &["x", "w", "b"], &["y"], &[attr_ints("kernel_shape", &[1, 1])])],
            &[tensor_f32("w", &[5, 3, 1, 1], &ws), tensor_f32("b", &[5], &[0.5, -1.0, 0.0, 2.0, 0.25])],
            &["x"],
            &["y"],
        );
        let g = graph(&m).unwrap();
        let x = Tensor::from_vec(xs, (2, 3, 4, 6), &Device::Cpu).unwrap();
        let w = Tensor::from_vec(ws, (5, 3, 1, 1), &Device::Cpu).unwrap();
        let b = Tensor::from_vec(vec![0.5f32, -1.0, 0.0, 2.0, 0.25], (1, 5, 1, 1), &Device::Cpu).unwrap();
        let want: Vec<f32> = x.conv2d(&w, 0, 1, 1, 1).unwrap().broadcast_add(&b).unwrap().flatten_all().unwrap().to_vec1().unwrap();
        let got = run1(&g, x);
        assert_eq!(got.len(), want.len());
        assert!(got.iter().zip(&want).all(|(a, b)| (a - b).abs() < 1e-4), "{got:?} vs {want:?}");
    }

    #[test]
    fn unequal_strides_subsample_rows_only() {
        // 1×1 conv (weight 1) with strides [2, 1] on a 4×2 input: rows 0 and 2, both columns
        let m = model(
            &[node("Conv", &["x", "w"], &["y"], &[attr_ints("kernel_shape", &[1, 1]), attr_ints("strides", &[2, 1])])],
            &[tensor_f32("w", &[1, 1, 1, 1], &[1.0])],
            &["x"],
            &["y"],
        );
        let g = graph(&m).unwrap();
        let x = Tensor::from_vec((0..8).map(|v| v as f32).collect::<Vec<_>>(), (1, 1, 4, 2), &Device::Cpu).unwrap();
        assert_eq!(run1(&g, x), vec![0., 1., 4., 5.]);
    }

    #[test]
    fn hard_sigmoid_erf_and_layer_norm_ops() {
        let m = model(
            &[
                node("HardSigmoid", &["x"], &["h"], &[]),
                node("ReduceMean", &["h"], &["m"], &[attr_ints("axes", &[-1])]),
                node("Sub", &["h", "m"], &["d"], &[]),
                node("Pow", &["d", "two"], &["p"], &[]),
                node("Erf", &["p"], &["e"], &[]),
                node("Sqrt", &["e"], &["s"], &[]),
                node("Div", &["s", "two"], &["y"], &[]),
            ],
            &[tensor_f32("two", &[], &[2.0])],
            &["x"],
            &["y"],
        );
        let g = graph(&m).unwrap();
        let x = Tensor::from_vec(vec![-10f32, 0., 10.], (1, 3), &Device::Cpu).unwrap();
        let y = run1(&g, x);
        // hard sigmoid (0.2x + 0.5, clamped): 0, 0.5, 1; mean 0.5; squared deviations 0.25, 0, 0.25
        let want: Vec<f32> = [0.25f32, 0.0, 0.25].iter().map(|v| libm_erf(*v).sqrt() / 2.0).collect();
        for (a, b) in y.iter().zip(&want) {
            assert!((a - b).abs() < 1e-5, "{y:?} vs {want:?}");
        }
    }

    /// erf by its series (enough for small arguments).
    fn libm_erf(x: f32) -> f32 {
        let x = f64::from(x);
        let mut sum = 0.0;
        let mut term = x;
        for n in 0..30 {
            sum += term / (2 * n + 1) as f64;
            term *= -x * x / (n + 1) as f64;
        }
        (sum * 2.0 / std::f64::consts::PI.sqrt()) as f32
    }

    #[test]
    fn softmax_before_opset_13_flattens_the_trailing_axes() {
        let m = model(&[node("Softmax", &["x"], &["y"], &[attr_int("axis", 1)])], &[], &["x"], &["y"]);
        let g = graph(&m).unwrap();
        let x = Tensor::zeros((1, 2, 2), candle_core::DType::F32, &Device::Cpu).unwrap();
        // the four values share one softmax (opset 11): 0.25 each, not 0.5
        assert_eq!(run1(&g, x), vec![0.25; 4]);
    }

    /// A Conv node over `x` with weights of `shape` (ones) and the given attributes.
    fn conv(shape: &[i64], attrs: &[Vec<u8>]) -> Vec<u8> {
        let n = shape.iter().product::<i64>().max(0) as usize;
        model(&[node("Conv", &["x", "w"], &["y"], attrs)], &[tensor_f32("w", shape, &vec![1.0; n])], &["x"], &["y"])
    }

    /// A hostile model file: a zero-size kernel, a huge dilation or padding are refused at load, and an input smaller than
    /// the window is an error, never a panic or an overflow.
    #[test]
    fn hostile_convolution_shapes_are_errors() {
        let same = attr_str("auto_pad", "SAME_UPPER");
        for (shape, attrs) in [
            (vec![1, 1, 0, 3], vec![same.clone()]),
            (vec![1, 1, 3, 0], vec![]),
            (vec![1, 1, 5, 5], vec![same.clone(), attr_ints("dilations", &[i64::MAX / 2, i64::MAX / 2])]),
            (vec![1, 1, 3, 3], vec![attr_ints("dilations", &[1 << 20, 1 << 20])]),
            (vec![1, 1, 3, 3], vec![attr_ints("pads", &[1 << 40, 0, 0, 0])]),
        ] {
            assert!(graph(&conv(&shape, &attrs)).is_err(), "{shape:?}");
        }
        // a feature map smaller than the window, on the general path (and a strided one): an error at run time
        let x = Tensor::ones((1, 1, 2, 2), candle_core::DType::F32, &Device::Cpu).unwrap();
        for attrs in [vec![], vec![attr_ints("strides", &[2, 1])], vec![attr_ints("dilations", &[2, 2])]] {
            let g = graph(&conv(&[1, 1, 3, 3], &attrs)).unwrap();
            assert!(g.run(&x).is_err());
        }
        // padded up to the window it runs
        let g = graph(&conv(&[1, 1, 3, 3], &[attr_ints("pads", &[1, 1, 1, 1])])).unwrap();
        assert_eq!(run1(&g, x), vec![4.0; 4]);
    }

    /// A kernel too large to pad for is refused at load, not left to exhaust memory in a pad.
    #[test]
    fn a_huge_kernel_is_refused_at_load() {
        // pooling: the kernel comes straight from an attribute
        let pool = model(
            &[node("MaxPool", &["x"], &["y"], &[attr_ints("kernel_shape", &[1 << 40, 1]), attr_str("auto_pad", "SAME_UPPER")])],
            &[],
            &["x"],
            &["y"],
        );
        assert!(graph(&pool).is_err());
        // a convolution: the kernel comes from the weights
        assert!(graph(&conv(&[1, 1, 2000, 1], &[attr_str("auto_pad", "SAME_UPPER")])).is_err());
        // an ordinary one still loads
        assert!(graph(&conv(&[1, 1, 3, 3], &[attr_str("auto_pad", "SAME_UPPER")])).is_ok());
    }

    /// A stride is bounded at load, like a dilation or a pad: the depthwise path allocates one phase per stride step, a
    /// ceil-mode pool pads by it and a transposed convolution's output grows with it.
    #[test]
    fn a_huge_stride_is_refused_at_load() {
        for strides in [[1 << 20, 1 << 20], [1 << 32, 1 << 32], [65, 1]] {
            let attrs = [attr_int("group", 4), attr_ints("strides", &strides)];
            assert!(graph(&conv(&[4, 1, 3, 3], &attrs)).is_err(), "depthwise {strides:?}");
            let pool = model(
                &[node("MaxPool", &["x"], &["y"], &[attr_ints("kernel_shape", &[2, 2]), attr_ints("strides", &strides), attr_int("ceil_mode", 1)])],
                &[],
                &["x"],
                &["y"],
            );
            assert!(graph(&pool).is_err(), "pool {strides:?}");
            let up = model(
                &[node("ConvTranspose", &["x", "w"], &["y"], &[attr_ints("strides", &strides)])],
                &[tensor_f32("w", &[1, 1, 2, 2], &[1.0; 4])],
                &["x"],
                &["y"],
            );
            assert!(graph(&up).is_err(), "transposed {strides:?}");
        }
        // the stride-2 layers of the real models still load
        let attrs = [attr_int("group", 4), attr_ints("strides", &[2, 2])];
        assert!(graph(&conv(&[4, 1, 3, 3], &attrs)).is_ok());
        let up = model(
            &[node("ConvTranspose", &["x", "w"], &["y"], &[attr_ints("strides", &[2, 2])])],
            &[tensor_f32("w", &[1, 1, 2, 2], &[1.0; 4])],
            &["x"],
            &["y"],
        );
        assert!(graph(&up).is_ok());
    }

    /// The ceil-mode pad is sized by the stride: with a window that load would refuse, `pool` itself must still return an
    /// error, not ask the allocator for terabytes.
    #[test]
    fn a_ceil_mode_pool_pad_is_checked() {
        let win = super::Window { kernel: [1, 1], strides: [1 << 20, 1 << 20], dilation: 1, pads: super::Pads::Fixed([0; 4]) };
        let x = Tensor::ones((1, 1, 2, 2), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(super::pool(&Budget::new(usize::MAX), &x, &win, true, true).is_err());
    }

    /// Shape's start / end (opset 15) would slice the dims; they are not implemented, so they fail at load.
    #[test]
    fn shape_with_start_or_end_is_refused_at_load() {
        for attr in [attr_int("start", 1), attr_int("end", 2)] {
            let m = model(&[node("Shape", &["x"], &["s"], &[attr])], &[], &["x"], &["s"]);
            assert!(graph(&m).is_err());
        }
        let m = model(&[node("Shape", &["x"], &["s"], &[])], &[], &["x"], &["s"]);
        assert!(graph(&m).is_ok());
    }

    /// A model-sized initializer: `n` ones.
    fn ones(name: &str, dims: &[i64]) -> Vec<u8> {
        let n = dims.iter().product::<i64>().max(0) as usize;
        tensor_f32(name, dims, &vec![1.0; n])
    }

    /// Just above `MAX_RUNTIME_ELEMENTS` squared-root (2^15), so a product of two of them is over the limit.
    const OVER: i64 = 32769;

    /// Broadcasting arithmetic: two small initializers whose per-axis maximum is over the limit are an error, not a
    /// gigabyte-scale allocation (at [65536, 1] and [1, 65536] it is 16 GB and candle aborts the process).
    #[test]
    fn a_broadcast_output_over_the_limit_is_an_error() {
        let x = Tensor::zeros((1, 1), candle_core::DType::F32, &Device::Cpu).unwrap();
        for op in ["Add", "Sub", "Mul", "Div", "Pow"] {
            let m = model(&[node(op, &["a", "b"], &["y"], &[])], &[ones("a", &[OVER, 1]), ones("b", &[1, OVER])], &["x"], &["y"]);
            assert!(graph(&m).unwrap().run(&x).is_err(), "{op}");
        }
        // and an ordinary broadcast still runs
        let m = model(&[node("Add", &["a", "b"], &["y"], &[])], &[ones("a", &[3, 1]), ones("b", &[1, 4])], &["x"], &["y"]);
        assert_eq!(run1(&graph(&m).unwrap(), x), vec![2.0; 12]);
    }

    /// MatMul: the output [m, n] (and the broadcast operands) are checked before candle multiplies.
    #[test]
    fn a_matmul_output_over_the_limit_is_an_error() {
        let x = Tensor::zeros((1, 1), candle_core::DType::F32, &Device::Cpu).unwrap();
        let m = model(&[node("MatMul", &["a", "b"], &["y"], &[])], &[ones("a", &[OVER, 1]), ones("b", &[1, OVER])], &["x"], &["y"]);
        assert!(graph(&m).unwrap().run(&x).is_err());
        // mismatched inner sizes are an error too
        let m = model(&[node("MatMul", &["a", "b"], &["y"], &[])], &[ones("a", &[2, 3]), ones("b", &[4, 2])], &["x"], &["y"]);
        assert!(graph(&m).unwrap().run(&x).is_err());
        let m = model(&[node("MatMul", &["a", "b"], &["y"], &[])], &[ones("a", &[2, 3]), ones("b", &[3, 4])], &["x"], &["y"]);
        assert_eq!(run1(&graph(&m).unwrap(), x), vec![3.0; 8]);
    }

    /// `n` MatMuls of the same two small initializers, each result a graph output under its own name: every output is
    /// `side²` elements, well under any per-allocation ceiling, and none can be freed after its last reader.
    fn retained_matmuls(n: usize, side: i64) -> Vec<u8> {
        let names: Vec<String> = (0..n).map(|i| format!("y{i}")).collect();
        let nodes: Vec<Vec<u8>> = names.iter().map(|y| node("MatMul", &["a", "b"], &[y.as_str()], &[])).collect();
        let outs: Vec<&str> = names.iter().map(String::as_str).collect();
        model(&nodes, &[ones("a", &[side, 1]), ones("b", &[1, side])], &["x"], &outs)
    }

    /// The aggregate budget: results a run keeps (graph outputs are never freed) count against what it may hold at once,
    /// so repeating one cheap node cannot add up to an out-of-memory abort.
    #[test]
    fn retained_outputs_are_bounded_by_the_live_budget() {
        let x = Tensor::zeros((1, 1), candle_core::DType::F32, &Device::Cpu).unwrap();
        // ten outputs of 200 × 200 = 40 000 elements, plus the 1-element input: 400 001 held at the end
        let g = graph(&retained_matmuls(10, 200)).unwrap();
        let (outs, peak) = g.run_within(&x, 500_000).unwrap();
        assert_eq!((outs.len(), peak), (10, 400_001));
        // the eighth would take the run to 320 001 elements, over 300 000: an error naming the node, before it is stored
        let e = g.run_within(&x, 300_000).unwrap_err().to_string();
        assert!(e.contains("MatMul") && e.contains("at once"), "{e}");
    }

    /// Values freed after their last reader stop counting, and the peak is what the run held together.
    #[test]
    fn freed_values_do_not_count_against_the_live_budget() {
        let x = Tensor::zeros((1, 1), candle_core::DType::F32, &Device::Cpu).unwrap();
        let nodes = [node("Add", &["a", "b"], &["y0"], &[]), node("Relu", &["y0"], &["y1"], &[]), node("Relu", &["y1"], &["y2"], &[])];
        let m = model(&nodes, &[ones("a", &[3, 1]), ones("b", &[1, 4])], &["x"], &["y2"]);
        let g = graph(&m).unwrap();
        // input 1 + two live 12-element tensors at the most: 25, not the 37 of all three results together
        assert_eq!(g.run_within(&x, 25).unwrap().1, 25);
        assert!(g.run_within(&x, 24).is_err());
    }

    /// A padded convolution holds its padded copy of the input, the copy candle lays out for it, the im2col scratch and
    /// its output at once: 64 (input) + 121 + 121 + 729 + 81 = 1116 elements here. Each alone fits in 900, together they
    /// do not, so the run is refused before the convolution starts.
    #[test]
    fn a_padded_convolution_reserves_its_copies_scratch_and_output_together() {
        let x = Tensor::ones((1, 1, 8, 8), candle_core::DType::F32, &Device::Cpu).unwrap();
        // pads (top, left, bottom, right) of (1, 1, 2, 2): not uniform, so the padded copy is made here
        let g = graph(&conv(&[1, 1, 3, 3], &[attr_ints("pads", &[1, 1, 2, 2])])).unwrap();
        let e = g.run_within(&x, 900).expect_err("the copies, scratch and output together are over 900").to_string();
        assert!(e.contains("Conv") && e.contains("900"), "{e}");
        let (outs, peak) = g.run_within(&x, 1300).unwrap();
        assert_eq!((outs[0].dims(), peak), (&[1, 1, 9, 9][..], 1116));
    }

    /// A padded max pool holds the padded copy and its output together: 100 (input) + 120 + 100 = 320 elements here, over
    /// 300 although the copy (220 with the input) and the output (200) each fit.
    #[test]
    fn a_padded_max_pool_reserves_its_padded_copy_and_output_together() {
        let x = Tensor::ones((1, 1, 10, 10), candle_core::DType::F32, &Device::Cpu).unwrap();
        let m =
            model(&[node("MaxPool", &["x"], &["y"], &[attr_ints("kernel_shape", &[1, 3]), attr_ints("pads", &[0, 1, 0, 1])])], &[], &["x"], &["y"]);
        let g = graph(&m).unwrap();
        let e = g.run_within(&x, 300).expect_err("the padded copy and the output together are over 300").to_string();
        assert!(e.contains("MaxPool") && e.contains("300"), "{e}");
        let (outs, peak) = g.run_within(&x, 400).unwrap();
        assert_eq!((outs[0].dims(), peak), (&[1, 1, 10, 10][..], 320));
    }

    /// An operator that is refused partway gives back everything it reserved: the budget holds what the run held before it.
    #[test]
    fn a_refused_operator_releases_what_it_reserved() {
        let x = Value::F(Tensor::ones((1, 1, 8, 8), candle_core::DType::F32, &Device::Cpu).unwrap());
        let w = Value::F(Tensor::ones((1, 1, 3, 3), candle_core::DType::F32, &Device::Cpu).unwrap());
        let window = super::Window { kernel: [3, 3], strides: [1, 1], dilation: 1, pads: super::Pads::Fixed([1, 1, 2, 2]) };
        let op = Op::Conv { window, groups: 1 };
        // the run holds its input (64); the convolution reserves its padded copy and its output, then is refused at the
        // next buffer (the input laid out for the product): over 300
        let budget = Budget::new(300);
        budget.hold(64).unwrap();
        let e = exec(&budget, &op, &[Some(&x), Some(&w)]).unwrap_err().to_string();
        assert!(e.contains("at once") && e.contains("300"), "{e}");
        assert_eq!(budget.held(), 64);
        assert!(budget.peak() > 64 + 121, "the padded copy and the output were reserved before the refusal: {}", budget.peak());
        // the same operator within the limit gives its reservations back too: only the run's own 64 is held
        let budget = Budget::new(2000);
        budget.hold(64).unwrap();
        assert!(exec(&budget, &op, &[Some(&x), Some(&w)]).is_ok());
        assert_eq!(budget.held(), 64);
    }

    /// BatchNormalization: per-channel parameters that are not one value per channel would broadcast the output to the
    /// parameters' size (a one-channel input with `K` parameters becomes `K` channels).
    #[test]
    fn batch_norm_parameters_must_match_the_channels() {
        let bn = |k: i64| {
            let names = ["s", "b", "m", "v"];
            let inits: Vec<Vec<u8>> = names.iter().map(|n| ones(n, &[k])).collect();
            model(&[node("BatchNormalization", &["x", "s", "b", "m", "v"], &["y"], &[])], &inits, &["x"], &["y"])
        };
        let x = Tensor::zeros((1, 1, 2, 2), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(graph(&bn(3)).unwrap().run(&x).is_err());
        assert!(graph(&bn(OVER)).unwrap().run(&x).is_err());
        assert_eq!(graph(&bn(1)).unwrap().run(&x).unwrap()[0].dims(), &[1, 1, 2, 2]);
        // parameters of rank 2 whose quotient would broadcast
        let m = model(
            &[node("BatchNormalization", &["x", "s", "b", "m", "v"], &["y"], &[])],
            &[ones("s", &[OVER, 1]), ones("b", &[1]), ones("m", &[1]), ones("v", &[1, OVER])],
            &["x"],
            &["y"],
        );
        assert!(graph(&m).unwrap().run(&x).is_err());
    }

    /// A convolution's output is `c_out` channels (from the weights) of the input's size: over the limit it is an error,
    /// as is a bias that is not one value per output channel.
    #[test]
    fn a_convolution_output_over_the_limit_is_an_error() {
        let x = Tensor::zeros((1, 1, 512, 512), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(graph(&conv(&[OVER, 1, 1, 1], &[])).unwrap().run(&x).is_err());
        // a 3×3 one too: the same output, built through im2col
        assert!(graph(&conv(&[OVER, 1, 3, 3], &[attr_str("auto_pad", "SAME_UPPER")])).unwrap().run(&x).is_err());
        // one bias value on a one-channel output is fine, three are not
        let with_bias = |k: i64| model(&[node("Conv", &["x", "w", "b"], &["y"], &[])], &[ones("w", &[1, 1, 1, 1]), ones("b", &[k])], &["x"], &["y"]);
        let small = Tensor::zeros((1, 1, 2, 2), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(graph(&with_bias(3)).unwrap().run(&small).is_err());
        assert_eq!(graph(&with_bias(1)).unwrap().run(&small).unwrap()[0].dims(), &[1, 1, 2, 2]);
    }

    #[test]
    fn a_transposed_convolution_output_over_the_limit_is_an_error() {
        let x = Tensor::zeros((1, 1, 512, 512), candle_core::DType::F32, &Device::Cpu).unwrap();
        let up = |w: &[i64]| model(&[node("ConvTranspose", &["x", "w"], &["y"], &[])], &[ones("w", w)], &["x"], &["y"]);
        assert!(graph(&up(&[1, OVER, 1, 1])).unwrap().run(&x).is_err());
        assert_eq!(graph(&up(&[1, 2, 2, 2])).unwrap().run(&x).unwrap()[0].dims(), &[1, 2, 513, 513]);
    }

    /// The scratch ceiling is higher than the output ceiling: the medium detector's Conv.121 on the 1600 × 1216 HierText
    /// photo (1 × 400 × 304 × 256 × 3 × 3 = 280 million im2col elements) and on the
    /// reader's largest photo (4000 × 4000: 2.304 billion) are accepted, and a buffer far above the ceiling is refused.
    /// Checked on the bound itself: running such a convolution would allocate over a gigabyte.
    #[test]
    fn the_shipped_models_largest_scratch_buffers_are_within_the_scratch_limit() {
        assert_eq!(checked_scratch(&[1, 400, 304, 256, 3, 3]).unwrap(), 280_166_400);
        assert_eq!(checked_scratch(&[1, 1000, 1000, 256, 3, 3]).unwrap(), 2_304_000_000);
        // the transposed convolution's column buffer of the same kind
        assert!(checked_scratch(&[1, 64, 3, 3, 1000, 1000]).is_ok());
        // the medium detector's measured peak at 4000 × 4000 (the Conv.121 buffer and everything held with it), with the
        // 1.5 × margin the limit keeps over it
        const { assert!(2_880_000_000 * 3 / 2 <= super::MAX_LIVE_ELEMENTS) };
        assert!(checked_scratch(&[1, 65_536, 65_536, 16, 3, 3]).is_err());
        assert!(checked_scratch(&[usize::MAX, 2]).is_err());
        // outputs keep the lower ceiling
        assert!(checked_elements(&[1, 128, 2001, 2001]).is_ok());
        assert!(checked_elements(&[1, 65_536, 65_536]).is_err());
    }

    /// A convolution whose im2col buffer is over the scratch ceiling while its output is small (many input channels and a
    /// big kernel over a modest map) is an error, not an allocation of tens of gigabytes.
    #[test]
    fn a_convolution_scratch_buffer_over_the_limit_is_an_error() {
        let x = Tensor::zeros((1, 64, 300, 300), candle_core::DType::F32, &Device::Cpu).unwrap();
        // output 1 × 1 × 268 × 268, im2col 268 · 268 · 64 · 33 · 33 = 5.0 billion
        assert!(graph(&conv(&[1, 64, 33, 33], &[])).unwrap().run(&x).is_err());
    }

    /// Concat: the output is the sum of its inputs, and the same value may be named any number of times.
    #[test]
    fn a_concat_over_the_limit_is_an_error() {
        let n = 1usize << 25;
        let x = Tensor::zeros((1, n), candle_core::DType::F32, &Device::Cpu).unwrap();
        let names = ["x"; 33];
        let m = model(&[node("Concat", &names, &["y"], &[attr_int("axis", 1)])], &[], &["x"], &["y"]);
        assert!(graph(&m).unwrap().run(&x).is_err());
        // a shape vector doubled by Concat, node after node, is bounded too
        let (a, b) = (Value::I(vec![0; 40_000]), Value::I(vec![0; 40_000]));
        assert!(exec(&Budget::new(usize::MAX), &Op::Concat { axis: 0 }, &[Some(&a), Some(&b)]).is_err());
        let small = Value::I(vec![1, 2]);
        assert!(exec(&Budget::new(usize::MAX), &Op::Concat { axis: 0 }, &[Some(&small), Some(&small)]).is_ok());
    }

    /// Resize: the intermediate (rows picked, columns not yet) is checked as well as the result, and so is an output side
    /// of an input with a zero dimension (no elements, so the product alone doesn't bound it).
    #[test]
    fn a_resize_intermediate_over_the_limit_is_an_error() {
        let x = Tensor::zeros((1, 1, 1, 32768), candle_core::DType::F32, &Device::Cpu).unwrap();
        let x = Value::F(x);
        let sizes = Value::I(vec![1, 1, OVER, 1]);
        assert!(exec(&Budget::new(usize::MAX), &Op::Resize, &[Some(&x), None, None, Some(&sizes)]).is_err());
        let empty = Value::F(Tensor::zeros((0, 1, 1, 1), candle_core::DType::F32, &Device::Cpu).unwrap());
        let sizes = Value::I(vec![0, 1, 1 << 40, 1]);
        assert!(exec(&Budget::new(usize::MAX), &Op::Resize, &[Some(&empty), None, None, Some(&sizes)]).is_err());
    }

    /// A hostile transposed convolution: weights that are not a constant 4-D tensor or have an empty kernel fail at load; a
    /// zero-sized or too-large input is an error before candle's unchecked output-size arithmetic.
    #[test]
    fn hostile_transposed_convolutions_are_errors() {
        let up = |w_shape: &[i64], attrs: &[Vec<u8>]| {
            let n = w_shape.iter().product::<i64>().max(0) as usize;
            model(&[node("ConvTranspose", &["x", "w"], &["y"], attrs)], &[tensor_f32("w", w_shape, &vec![1.0; n])], &["x"], &["y"])
        };
        assert!(graph(&up(&[1, 1, 0, 2], &[])).is_err(), "an empty kernel");
        assert!(graph(&up(&[1, 2, 2], &[])).is_err(), "weights that are not 4-D");
        let no_weights = model(&[node("ConvTranspose", &["x", "w"], &["y"], &[])], &[], &["x", "w"], &["y"]);
        assert!(graph(&no_weights).is_err(), "weights that are not constants");
        // a feature map with no rows
        let g = graph(&up(&[1, 1, 2, 2], &[])).unwrap();
        let empty = Tensor::zeros((1, 1, 0, 4), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(g.run(&empty).is_err());
        // an output of more than the runtime limit: 599 × 64 + 2 = 38338 rows and columns
        let g = graph(&up(&[1, 1, 2, 2], &[attr_ints("strides", &[64, 64])])).unwrap();
        let big = Tensor::zeros((1, 1, 600, 600), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert!(g.run(&big).is_err());
        // an ordinary one runs: 2 × 2 weights of 1 over a 2 × 2 map of 1, stride 1
        let g = graph(&up(&[1, 1, 2, 2], &[])).unwrap();
        let x = Tensor::ones((1, 1, 2, 2), candle_core::DType::F32, &Device::Cpu).unwrap();
        assert_eq!(run1(&g, x), vec![1., 2., 1., 2., 4., 2., 1., 2., 1.]);
    }

    #[test]
    fn unknown_operators_fail_at_load() {
        let m = model(&[node("NonMaxSuppression", &["x"], &["y"], &[])], &[], &["x"], &["y"]);
        assert!(graph(&m).err().unwrap().to_string().contains("not supported"));
        // a value used before it is computed
        let m = model(&[node("Relu", &["z"], &["y"], &[])], &[], &["x"], &["y"]);
        assert!(graph(&m).is_err());
    }
}
