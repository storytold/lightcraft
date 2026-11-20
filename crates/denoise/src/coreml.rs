//! A denoise network as a Core ML model, so the Neural Engine can run it.
//!
//! Core ML takes no ONNX, and the Neural Engine computes in 16-bit floats (largest value 65504): some denoisers' activations
//! reach 10⁵ and their output is a million times the input (RawNIND's), which comes back as NaN from 16-bit
//! arithmetic. So the network is first [`rescale`]d: convolutions are linear and a leaky ReLU or a maximum commutes
//! with a positive factor, so each layer's weights and bias can absorb a power-of-two factor (a power of two is exact
//! in any float format, so in real arithmetic nothing changes) that brings that layer's activations down to a safe
//! size, and the model's answer comes out a known power of two smaller ([`Built::output_scale`], which the caller
//! divides out again). How far the activations reach is measured on a tile (`CpuRunner::peaks`,
//! [`crate::reference::peaks`]); the 16-bit arithmetic itself is not exact, and [`fp16_report`] says how many of the
//! rescaled weights it rounds to the coarse end of its range.
//!
//! Then [`spec`] writes the network as a Core ML `NeuralNetwork` model file (`.mlmodel`, a protobuf) with this
//! file's own small encoder: no Core ML tools, no Python, no generated code. The file is compiled and run by
//! `lightcraft-coreml`.

use crate::net::{Conv, ConvT2, Net, NetError, Op, TensorId};

/// Name of the model's input array.
pub const INPUT: &str = "input";
/// Name of the model's output array.
pub const OUTPUT: &str = "output";

/// The largest value a layer's *output before its activation* may reach after [`rescale`] on the tile the peaks were
/// measured on. A 16-bit float tops out at 65504, so this leaves a factor 16 for tiles brighter or noisier than the
/// measured one (the check tile has the whole range, from black to clipped white, so that is little in practice). The
/// peaks are measured after the activation; before a leaky ReLU a value can be `1 / alpha` times larger than what
/// follows it, which [`rescale`] allows for.
pub const TARGET_PEAK: f32 = 4096.0;

/// What [`build`] makes.
pub struct Built {
    /// The `.mlmodel` file's bytes.
    pub mlmodel: Vec<u8>,
    /// The factor (a power of two, at most 1) the model's answer is smaller by than the network's: divide by it.
    pub output_scale: f32,
    /// How the network's weights and biases sit in 16-bit range before the rescale, and after it.
    pub before: Fp16Report,
    pub after: Fp16Report,
}

/// How a network's weights and biases sit in the range of a 16-bit float: its normal numbers start at 6.1·10⁻⁵ (below
/// that the precision falls away, below 6·10⁻⁸ a number is rounded to zero).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fp16Report {
    /// Non-zero weights and biases.
    pub nonzero: usize,
    /// Of those, the ones below the normal range (with less precision than 16 bits give the rest).
    pub subnormal: usize,
    /// Of those, the ones that round to zero.
    pub lost: usize,
}

impl Fp16Report {
    /// The share of the non-zero numbers that have less than full 16-bit precision.
    pub fn subnormal_share(&self) -> f64 {
        if self.nonzero == 0 { 0.0 } else { (self.subnormal + self.lost) as f64 / self.nonzero as f64 }
    }
}

/// Count how `net`'s weights and biases sit in 16-bit range.
pub fn fp16_report(net: &Net) -> Fp16Report {
    const MIN_NORMAL: f32 = 6.103_515_6e-5;
    const MIN_SUBNORMAL: f32 = 5.960_464_5e-8;
    let mut r = Fp16Report::default();
    for op in net.ops() {
        let (w, b) = match op {
            Op::Conv(c) => (&c.weight, &c.bias),
            Op::ConvT2(c) => (&c.weight, &c.bias),
            _ => continue,
        };
        for v in w.iter().chain(b).map(|v| v.abs()).filter(|v| *v > 0.0) {
            r.nonzero += 1;
            if v < MIN_SUBNORMAL / 2.0 {
                r.lost += 1;
            } else if v < MIN_NORMAL {
                r.subnormal += 1;
            }
        }
    }
    r
}

/// An invalid-network error with this reason.
fn invalid<T>(why: impl Into<String>) -> Result<T, NetError> {
    Err(NetError::Invalid(why.into()))
}

/// The network, rescaled for 16-bit arithmetic, and the factor its output is then smaller by. `peaks[t]` is the
/// largest absolute value of tensor `t` (0 is the input) on a representative tile.
pub fn rescale(net: &Net, peaks: &[f32]) -> Result<(Net, f32), NetError> {
    if peaks.len() != net.ops().len() + 1 || peaks.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return invalid("the activation peaks do not match the network");
    }
    let mut scale = vec![1.0f32];
    let mut ops = Vec::with_capacity(net.ops().len());
    for (i, op) in net.ops().iter().enumerate() {
        // the peak is measured after the activation: before a leaky ReLU a value can be 1 / alpha times larger
        let leaky = match op {
            Op::Conv(c) => c.leaky,
            Op::ConvT2(c) => c.leaky,
            _ => None,
        };
        let peak = peaks.get(i + 1).copied().unwrap_or(0.0) / leaky.map_or(1.0, |a| a.abs().clamp(1.0 / 64.0, 1.0));
        let of = |t: TensorId| scale.get(t).copied().ok_or_else(|| NetError::Invalid("a layer reads a tensor that does not exist yet".into()));
        let (scaled, s) = match op {
            Op::Conv(c) => {
                let out = safe_scale(peak);
                let first = net.shape(c.src).map_or(0, |s| s.channels);
                // each source's channels bring their own scale: the weights bring them to the output's
                let mut parts = vec![(0..first, out / of(c.src)?)];
                if let Some(s2) = c.src2 {
                    parts.push((first..c.cin, out / of(s2)?));
                }
                let per_in = c.k * c.k;
                let mut weight = c.weight.clone();
                for row in weight.chunks_exact_mut(c.cin * per_in) {
                    for (range, factor) in &parts {
                        for ci in range.clone() {
                            if let Some(w) = row.get_mut(ci * per_in..(ci + 1) * per_in) {
                                w.iter_mut().for_each(|v| *v *= factor);
                            }
                        }
                    }
                }
                let bias = c.bias.iter().map(|b| b * out).collect();
                (Op::Conv(Conv { weight, bias, ..c.clone() }), out)
            }
            Op::ConvT2(c) => {
                let out = safe_scale(peak);
                let factor = out / of(c.src)?;
                let weight = c.weight.iter().map(|w| w * factor).collect();
                let bias = c.bias.iter().map(|b| b * out).collect();
                (Op::ConvT2(ConvT2 { weight, bias, ..c.clone() }), out)
            }
            // no weights to absorb a factor: the layer keeps its input's
            Op::MaxPool2 { src } | Op::DepthToSpace2 { src } => (op.clone(), of(*src)?),
        };
        ops.push(scaled);
        scale.push(s);
    }
    let out = scale.last().copied().unwrap_or(1.0);
    Ok((Net::new(net.in_channels, ops)?, out))
}

/// The largest power of two, at most 1, that brings `peak` down to [`TARGET_PEAK`] or less.
fn safe_scale(peak: f32) -> f32 {
    if peak <= TARGET_PEAK {
        return 1.0;
    }
    // peak is finite here; the exponent of a finite f32 is below 128
    let exponent = (peak / TARGET_PEAK).log2().ceil().clamp(0.0, 120.0) as i32;
    2f32.powi(-exponent)
}

/// The Core ML model for `net` on tiles of `tile × tile` cells, rescaled for 16-bit arithmetic with `peaks`
/// (see [`rescale`]).
pub fn build(net: &Net, tile: usize, peaks: &[f32]) -> Result<Built, NetError> {
    let (scaled, output_scale) = rescale(net, peaks)?;
    Ok(Built { mlmodel: spec(&scaled, tile)?, output_scale, before: fp16_report(net), after: fp16_report(&scaled) })
}

// ---------------------------------------------------------------------------------------------------------------------
// protobuf

mod pb {
    /// Appends `v` as a protobuf varint (seven bits per byte, low group first).
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

    /// Appends the key of a field: its number and wire type.
    fn tag(field: u32, wire: u8, out: &mut Vec<u8>) {
        varint((u64::from(field) << 3) | u64::from(wire), out);
    }

    /// A varint field (proto3: zero is left out).
    pub fn uint(field: u32, v: u64, out: &mut Vec<u8>) {
        if v != 0 {
            tag(field, 0, out);
            varint(v, out);
        }
    }

    /// Appends a boolean field (nothing is written when it is false).
    pub fn boolean(field: u32, v: bool, out: &mut Vec<u8>) {
        uint(field, u64::from(v), out);
    }

    /// A length-delimited field (always written, even when empty: an empty message is still present).
    pub fn bytes(field: u32, b: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }

    /// Appends a string field.
    pub fn string(field: u32, s: &str, out: &mut Vec<u8>) {
        bytes(field, s.as_bytes(), out);
    }

    /// Appends a 32-bit float field (nothing is written for zero, the default).
    pub fn float(field: u32, v: f32, out: &mut Vec<u8>) {
        if v != 0.0 {
            tag(field, 5, out);
            out.extend_from_slice(&v.to_le_bytes());
        }
    }

    /// A packed repeated integer field.
    pub fn packed(field: u32, values: &[u64], out: &mut Vec<u8>) {
        let mut body = Vec::new();
        values.iter().for_each(|&v| varint(v, &mut body));
        bytes(field, &body, out);
    }

    /// A packed repeated float field.
    pub fn packed_f32(field: u32, values: &[f32], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(values.len() as u64 * 4, out);
        out.reserve(values.len() * 4);
        values.iter().for_each(|v| out.extend_from_slice(&v.to_le_bytes()));
    }

    /// `message { field: inner }` as bytes.
    pub fn wrap(field: u32, inner: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        bytes(field, inner, &mut out);
        out
    }
}

// Core ML's `Model`, `NeuralNetwork`, and the layers used, by field number (Apple's coremltools protobuf files).
const MODEL_SPEC_VERSION: u32 = 1;
const MODEL_DESCRIPTION: u32 = 2;
const MODEL_NEURAL_NETWORK: u32 = 500;
const DESC_INPUT: u32 = 1;
const DESC_OUTPUT: u32 = 10;
const FEATURE_NAME: u32 = 1;
const FEATURE_TYPE: u32 = 3;
const FEATURE_MULTI_ARRAY: u32 = 5;
const ARRAY_SHAPE: u32 = 1;
const ARRAY_DATA_TYPE: u32 = 2;
const ARRAY_FLOAT32: u64 = 65568;
const NN_LAYERS: u32 = 1;
const NN_ARRAY_INPUT_SHAPE_MAPPING: u32 = 5;
const EXACT_ARRAY_MAPPING: u64 = 1;
const LAYER_NAME: u32 = 1;
const LAYER_INPUT: u32 = 2;
const LAYER_OUTPUT: u32 = 3;
const LAYER_CONVOLUTION: u32 = 100;
const LAYER_POOLING: u32 = 120;
const LAYER_ACTIVATION: u32 = 130;
const LAYER_CONCAT: u32 = 320;
const LAYER_REORGANIZE: u32 = 345;
const MODEL_SPECIFICATION_VERSION: u64 = 5;

/// `len` (a count) as a protobuf integer.
fn size(n: usize) -> u64 {
    n as u64
}

/// The Core ML name of tensor `t`: `input`, `output` for the last one, else a numbered name.
fn name_of(t: TensorId, last: TensorId) -> String {
    match t {
        0 => INPUT.to_string(),
        t if t == last => OUTPUT.to_string(),
        t => format!("t{t}"),
    }
}

/// `valid` padding with `pad` cells on every edge of a 2-D layer.
fn valid_padding(field: u32, pad: u64) -> Vec<u8> {
    let mut edge = Vec::new();
    pb::uint(1, pad, &mut edge);
    pb::uint(2, pad, &mut edge);
    let mut amounts = Vec::new();
    pb::bytes(10, &edge, &mut amounts);
    pb::bytes(10, &edge, &mut amounts);
    let mut valid = Vec::new();
    pb::bytes(1, &amounts, &mut valid);
    pb::wrap(field, &valid)
}

/// The weights message holding `values` as a packed float list.
fn weights(values: &[f32]) -> Vec<u8> {
    let mut w = Vec::new();
    pb::packed_f32(1, values, &mut w);
    w
}

/// One Core ML layer: its name, input and output names, the number of its kind's field and its parameters.
fn layer(name: &str, inputs: &[&str], output: &str, kind: u32, params: &[u8]) -> Vec<u8> {
    let mut l = Vec::new();
    pb::string(LAYER_NAME, name, &mut l);
    inputs.iter().for_each(|i| pb::string(LAYER_INPUT, i, &mut l));
    pb::string(LAYER_OUTPUT, output, &mut l);
    pb::bytes(kind, params, &mut l);
    l
}

/// What a convolution layer is made of.
struct ConvSpec<'a> {
    cout: usize,
    /// Input channels.
    cin: usize,
    k: usize,
    deconv: bool,
    weight: &'a [f32],
    bias: &'a [f32],
    /// A transposed convolution's output side, which Core ML wants spelled out.
    out_side: Option<usize>,
}

/// The parameters of a convolution layer, with its weights and bias.
fn convolution(c: &ConvSpec) -> Vec<u8> {
    let mut p = Vec::new();
    pb::uint(1, size(c.cout), &mut p);
    pb::uint(2, size(c.cin), &mut p);
    pb::uint(10, 1, &mut p);
    let (kernel, stride) = (size(c.k), if c.deconv { 2 } else { 1 });
    pb::packed(20, &[kernel, kernel], &mut p);
    pb::packed(30, &[stride, stride], &mut p);
    pb::packed(40, &[1, 1], &mut p);
    // "same" padding for the 3-wide kernels (the networks' convolutions are 1 or 3 wide), none for the rest
    p.extend(valid_padding(50, if c.deconv { 0 } else { size(c.k.saturating_sub(1) / 2) }));
    pb::boolean(60, c.deconv, &mut p);
    pb::boolean(70, true, &mut p);
    pb::bytes(90, &weights(c.weight), &mut p);
    pb::bytes(91, &weights(c.bias), &mut p);
    if let Some(side) = c.out_side {
        pb::packed(100, &[size(side), size(side)], &mut p);
    }
    p
}

/// The activation layer parameters of a leaky ReLU with slope `alpha`.
fn leaky_relu(alpha: f32) -> Vec<u8> {
    let mut leaky = Vec::new();
    pb::float(1, alpha, &mut leaky);
    let mut act = Vec::new();
    pb::bytes(15, &leaky, &mut act);
    act
}

/// A tensor's side in cells on a `tile`-wide input, from its resolution level.
fn side_at(tile: usize, level: i32) -> Result<usize, NetError> {
    let n = level.unsigned_abs();
    let side = if level >= 0 { tile.checked_shr(n) } else { 1usize.checked_shl(n).and_then(|m| tile.checked_mul(m)) };
    side.filter(|&s| s > 0).ok_or_else(|| NetError::Invalid("the tile does not fit the network".into()))
}

/// The network as a Core ML `NeuralNetwork` model file (`.mlmodel`), with the input `"input"`
/// (`[1, in_channels, tile, tile]`) and the output `"output"`, both 32-bit floats at the boundary.
pub fn spec(net: &Net, tile: usize) -> Result<Vec<u8>, NetError> {
    if !net.fits_tile(tile) {
        return invalid("the tile does not fit the network");
    }
    let last = net.output();
    let mut layers: Vec<Vec<u8>> = Vec::new();
    for (i, op) in net.ops().iter().enumerate() {
        let t = i + 1;
        let out = name_of(t, last);
        let shape = |id: TensorId| net.shape(id).ok_or_else(|| NetError::Invalid("a layer reads a tensor that does not exist yet".into()));
        // a layer's output before its leaky ReLU, when it has one
        let (pre, leaky) = match op {
            Op::Conv(Conv { leaky: Some(a), .. }) | Op::ConvT2(ConvT2 { leaky: Some(a), .. }) => (format!("t{t}_linear"), Some(*a)),
            _ => (out.clone(), None),
        };
        match op {
            Op::Conv(c) => {
                let a = name_of(c.src, last);
                let joined = match c.src2 {
                    Some(s2) => {
                        let joined = format!("t{t}_joined");
                        layers.push(layer(&format!("join{t}"), &[&a, &name_of(s2, last)], &joined, LAYER_CONCAT, &[]));
                        joined
                    }
                    None => a,
                };
                let params =
                    convolution(&ConvSpec { cout: c.cout, cin: c.cin, k: c.k, deconv: false, weight: &c.weight, bias: &c.bias, out_side: None });
                layers.push(layer(&format!("conv{t}"), &[&joined], &pre, LAYER_CONVOLUTION, &params));
            }
            Op::ConvT2(c) => {
                let side = side_at(tile, shape(t)?.level)?;
                let params =
                    convolution(&ConvSpec { cout: c.cout, cin: c.cin, k: 2, deconv: true, weight: &c.weight, bias: &c.bias, out_side: Some(side) });
                layers.push(layer(&format!("up{t}"), &[&name_of(c.src, last)], &pre, LAYER_CONVOLUTION, &params));
            }
            Op::MaxPool2 { src } => {
                let mut p = Vec::new();
                pb::packed(10, &[2, 2], &mut p);
                pb::packed(20, &[2, 2], &mut p);
                p.extend(valid_padding(30, 0));
                pb::boolean(50, true, &mut p);
                layers.push(layer(&format!("pool{t}"), &[&name_of(*src, last)], &out, LAYER_POOLING, &p));
            }
            Op::DepthToSpace2 { src } => {
                // "CRD" depth-to-space is Core ML's pixel shuffle
                let mut r = Vec::new();
                pb::uint(1, 2, &mut r);
                pb::uint(2, 2, &mut r);
                layers.push(layer(&format!("shuffle{t}"), &[&name_of(*src, last)], &out, LAYER_REORGANIZE, &r));
            }
        }
        if let Some(alpha) = leaky {
            layers.push(layer(&format!("act{t}"), &[&pre], &out, LAYER_ACTIVATION, &leaky_relu(alpha)));
        }
    }

    let mut nn = Vec::new();
    layers.iter().for_each(|l| pb::bytes(NN_LAYERS, l, &mut nn));
    pb::uint(NN_ARRAY_INPUT_SHAPE_MAPPING, EXACT_ARRAY_MAPPING, &mut nn);

    let feature = |name: &str, shape: &[usize]| {
        let mut array = Vec::new();
        if !shape.is_empty() {
            pb::packed(ARRAY_SHAPE, &shape.iter().map(|&d| size(d)).collect::<Vec<_>>(), &mut array);
        }
        pb::uint(ARRAY_DATA_TYPE, ARRAY_FLOAT32, &mut array);
        let mut ty = Vec::new();
        pb::bytes(FEATURE_MULTI_ARRAY, &array, &mut ty);
        let mut f = Vec::new();
        pb::string(FEATURE_NAME, name, &mut f);
        pb::bytes(FEATURE_TYPE, &ty, &mut f);
        f
    };
    let mut description = Vec::new();
    pb::bytes(DESC_INPUT, &feature(INPUT, &[1, net.in_channels, tile, tile]), &mut description);
    pb::bytes(DESC_OUTPUT, &feature(OUTPUT, &[]), &mut description);

    let mut model = Vec::new();
    pb::uint(MODEL_SPEC_VERSION, MODEL_SPECIFICATION_VERSION, &mut model);
    pb::bytes(MODEL_DESCRIPTION, &description, &mut model);
    pb::bytes(MODEL_NEURAL_NETWORK, &nn, &mut model);
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{onnx, reference, synthetic};

    /// A small network (convolutions, a pool, a concatenation, a transposed convolution and a depth-to-space) for the tests.
    fn net(tile: u64, ch: u64, depth: usize, seed: u64) -> Net {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lc-coreml-net-{}-{n}.onnx", std::process::id()));
        std::fs::write(&path, synthetic::unet_onnx(tile, ch, depth, seed)).unwrap();
        let n = onnx::read(&path).unwrap();
        let _ = std::fs::remove_file(path);
        n
    }

    /// A deterministic input for `net`.
    fn input(tile: usize, amplitude: f32) -> Vec<f32> {
        (0..4 * tile * tile).map(|i| amplitude * (0.5 + 0.5 * ((i * 7919 % 1009) as f32 / 1009.0))).collect()
    }

    /// Test: varints and fields are encoded as protobuf says.
    #[test]
    fn varints_and_fields_are_encoded_as_protobuf_says() {
        let mut out = Vec::new();
        pb::varint(300, &mut out);
        assert_eq!(out, [0xac, 0x02]);
        let mut out = Vec::new();
        pb::uint(1, 150, &mut out);
        assert_eq!(out, [0x08, 0x96, 0x01]);
        let mut out = Vec::new();
        pb::string(2, "testing", &mut out);
        assert_eq!(out, [0x12, 0x07, b't', b'e', b's', b't', b'i', b'n', b'g']);
        // zero is left out, an empty message is not
        let mut out = Vec::new();
        pb::uint(1, 0, &mut out);
        pb::float(2, 0.0, &mut out);
        assert!(out.is_empty());
        pb::bytes(320, &[], &mut out);
        assert_eq!(out, [0x82, 0x14, 0x00]);
        let mut out = Vec::new();
        pb::packed(30, &[2, 2], &mut out);
        assert_eq!(out, [0xf2, 0x01, 0x02, 0x02, 0x02]);
        let mut out = Vec::new();
        pb::float(1, 0.2, &mut out);
        assert_eq!(out, [0x0d, 0xcd, 0xcc, 0x4c, 0x3e]);
    }

    /// Test: a power of two scale brings a peak to the target.
    #[test]
    fn a_power_of_two_scale_brings_a_peak_to_the_target() {
        assert_eq!(safe_scale(0.0), 1.0);
        assert_eq!(safe_scale(TARGET_PEAK), 1.0);
        assert_eq!(safe_scale(TARGET_PEAK * 1.01), 0.5);
        assert_eq!(safe_scale(2.0e5), 2f32.powi(-6));
        assert!(2.0e5 * safe_scale(2.0e5) <= TARGET_PEAK);
        assert!(safe_scale(f32::MAX) > 0.0);
    }

    /// Test: rescaling changes nothing but the scale of the answer.
    #[test]
    fn rescaling_changes_nothing_but_the_scale_of_the_answer() {
        let n = net(32, 8, 2, 5);
        // a network that really grows: layers' weights scaled up, so peaks pass the target
        let mut ops = n.ops().to_vec();
        for op in &mut ops {
            match op {
                Op::Conv(c) => c.weight.iter_mut().for_each(|w| *w *= 6.0),
                Op::ConvT2(c) => c.weight.iter_mut().for_each(|w| *w *= 6.0),
                _ => {}
            }
        }
        let big = Net::new(n.in_channels, ops).unwrap();
        let x = input(32, 1.0);
        let peaks = reference::peaks(&big, 32, &x).unwrap();
        assert!(peaks.iter().any(|&p| p > 10.0 * TARGET_PEAK), "the test net must outgrow 16-bit comfort: {peaks:?}");
        let (scaled, out_scale) = rescale(&big, &peaks).unwrap();
        assert!(out_scale < 1.0 && out_scale.log2().fract() == 0.0);
        // every activation of the rescaled network is within the target on the same tile
        let after = reference::peaks(&scaled, 32, &x).unwrap();
        assert!(after.iter().all(|&p| p <= TARGET_PEAK * 1.001), "{after:?}");
        // and its answer is the original's times the scale (powers of two: the arithmetic is the same)
        let want = reference::run(&big, 32, &x).unwrap();
        let got = reference::run(&scaled, 32, &x).unwrap();
        let top = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        let worst = want.iter().zip(&got).fold(0f32, |m, (w, g)| m.max((w * out_scale - g).abs()));
        assert!(worst <= 1e-5 * top * out_scale, "off by {worst:e} of {top:e} x {out_scale}");
    }

    /// Test: a network that stays small is left alone.
    #[test]
    fn a_network_that_stays_small_is_left_alone() {
        let n = net(32, 4, 1, 3);
        let x = input(32, 0.2);
        let peaks = reference::peaks(&n, 32, &x).unwrap();
        assert!(peaks.iter().all(|&p| p < TARGET_PEAK), "{peaks:?}");
        let (scaled, out_scale) = rescale(&n, &peaks).unwrap();
        assert_eq!(out_scale, 1.0);
        assert_eq!(scaled, n);
    }

    /// Test: bad peaks are errors not panics.
    #[test]
    fn bad_peaks_are_errors_not_panics() {
        let n = net(32, 4, 1, 3);
        let len = n.ops().len() + 1;
        assert!(rescale(&n, &[]).is_err());
        assert!(rescale(&n, &vec![1.0; len + 1]).is_err());
        assert!(rescale(&n, &vec![f32::NAN; len]).is_err());
        assert!(rescale(&n, &vec![f32::INFINITY; len]).is_err());
        assert!(rescale(&n, &vec![-1.0; len]).is_err());
        assert!(spec(&n, 31).is_err());
        assert!(spec(&n, 0).is_err());
    }

    /// A protobuf message as its fields: `(number, varint or bytes)`. Enough of the wire format to read our own output
    /// back (no groups, no fixed-width fields but floats, which this reader keeps as bytes too).
    #[derive(Debug)]
    enum V {
        Int(u64),
        Bytes(Vec<u8>),
    }

    /// The fields of a protobuf message as `(number, wire type, bytes or value)`.
    fn fields(mut b: &[u8]) -> Vec<(u32, V)> {
        /// Reads a varint at the start of `b`: its value and its length.
        fn varint(b: &mut &[u8]) -> u64 {
            let (mut v, mut shift) = (0u64, 0);
            loop {
                let byte = b[0];
                *b = &b[1..];
                v |= u64::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    return v;
                }
                shift += 7;
            }
        }
        let mut out = Vec::new();
        while !b.is_empty() {
            let key = varint(&mut b);
            let (field, wire) = ((key >> 3) as u32, key & 7);
            out.push((
                field,
                match wire {
                    0 => V::Int(varint(&mut b)),
                    2 => {
                        let n = varint(&mut b) as usize;
                        let (body, rest) = b.split_at(n);
                        b = rest;
                        V::Bytes(body.to_vec())
                    }
                    5 => {
                        let (body, rest) = b.split_at(4);
                        b = rest;
                        V::Bytes(body.to_vec())
                    }
                    w => panic!("wire type {w}"),
                },
            ));
        }
        out
    }

    /// The bytes of field `n` of `msg`, if it has one.
    fn bytes_of(f: &[(u32, V)], n: u32) -> Vec<&[u8]> {
        f.iter().filter(|(k, _)| *k == n).filter_map(|(_, v)| if let V::Bytes(b) = v { Some(b.as_slice()) } else { None }).collect()
    }

    /// The integer of field `n` of `msg`, if it has one.
    fn int_of(f: &[(u32, V)], n: u32) -> Option<u64> {
        f.iter().find(|(k, _)| *k == n).and_then(|(_, v)| if let V::Int(i) = v { Some(*i) } else { None })
    }

    /// The text of field `n` of `msg`, if it has one.
    fn text(b: &[u8]) -> String {
        String::from_utf8(b.to_vec()).unwrap()
    }

    /// The numbers of the packed field `n` of `msg`.
    fn packed(b: &[u8]) -> Vec<u64> {
        let mut out = Vec::new();
        let mut b = b;
        while !b.is_empty() {
            let (mut v, mut shift) = (0u64, 0);
            loop {
                let byte = b[0];
                b = &b[1..];
                v |= u64::from(byte & 0x7f) << shift;
                if byte & 0x80 == 0 {
                    break;
                }
                shift += 7;
            }
            out.push(v);
        }
        out
    }

    /// The layer's kind: the number of its parameters' field (100 convolution, 120 pooling, 130 activation, 320
    /// concat, 345 reorganize data).
    fn kind(layer: &[(u32, V)]) -> u32 {
        layer.iter().map(|(k, _)| *k).find(|k| *k >= 100).unwrap()
    }

    /// The layers a network's operations are written as, by kind.
    fn expected_kinds(n: &Net) -> Vec<u32> {
        let mut want = Vec::new();
        for op in n.ops() {
            let (join, leaky) = match op {
                Op::Conv(c) => (c.src2.is_some(), c.leaky.is_some()),
                Op::ConvT2(c) => (false, c.leaky.is_some()),
                Op::MaxPool2 { .. } => {
                    want.push(120);
                    continue;
                }
                Op::DepthToSpace2 { .. } => {
                    want.push(345);
                    continue;
                }
            };
            want.extend(join.then_some(320));
            want.push(100);
            want.extend(leaky.then_some(130));
        }
        want
    }

    /// Every layer reads a tensor the input or an earlier layer made, and none is made twice; the last is the output.
    fn check_graph(layers: &[Vec<(u32, V)>]) {
        let mut known = vec![INPUT.to_string()];
        for l in layers {
            for i in bytes_of(l, 2) {
                assert!(known.contains(&text(i)), "{} reads {} before it exists", text(bytes_of(l, 1)[0]), text(i));
            }
            let out = text(bytes_of(l, 3)[0]);
            assert!(!known.contains(&out), "{out} is made twice");
            known.push(out);
        }
        assert_eq!(known.last().map(String::as_str), Some(OUTPUT));
    }

    /// `(cout, cin, kernel, transposed)` of a convolution operation.
    fn conv_dims(op: &Op) -> Option<(usize, usize, usize, bool)> {
        match op {
            Op::Conv(c) => Some((c.cout, c.cin, c.k, false)),
            Op::ConvT2(c) => Some((c.cout, c.cin, 2, true)),
            _ => None,
        }
    }

    /// Checks a convolution layer's parameters against the network's layer.
    fn check_conv(params: &[(u32, V)], (cout, cin, k, deconv): (usize, usize, usize, bool)) {
        assert_eq!((int_of(params, 1), int_of(params, 2)), (Some(cout as u64), Some(cin as u64)));
        assert_eq!(packed(bytes_of(params, 20)[0]), vec![k as u64; 2]);
        assert_eq!(packed(bytes_of(params, 30)[0]), vec![if deconv { 2 } else { 1 }; 2]);
        assert_eq!(int_of(params, 60).is_some(), deconv, "isDeconvolution");
        assert_eq!(int_of(params, 70), Some(1), "hasBias");
        // "same" padding by amounts: (k - 1) / 2 on each edge, none for a transposed convolution
        let amounts = fields(bytes_of(&fields(bytes_of(params, 50)[0]), 1)[0]);
        let edges = bytes_of(&amounts, 10);
        assert_eq!(edges.len(), 2);
        let pad = if deconv { 0 } else { (k as u64 - 1) / 2 };
        for e in edges {
            let e = fields(e);
            assert_eq!((int_of(&e, 1).unwrap_or(0), int_of(&e, 2).unwrap_or(0)), (pad, pad));
        }
        assert_eq!(bytes_of(params, 100).len(), usize::from(deconv), "a transposed convolution spells out its output size");
        assert_eq!(bytes_of(&fields(bytes_of(params, 90)[0]), 1)[0].len() / 4, cout * cin * k * k);
    }

    /// A 2 x 2 maximum, stride 2: the exclude-padding flag (50), not the global-pooling one (60).
    fn check_pool(params: &[(u32, V)]) {
        assert_eq!(int_of(params, 1).unwrap_or(0), 0, "maximum");
        assert_eq!(packed(bytes_of(params, 10)[0]), vec![2, 2]);
        assert_eq!(packed(bytes_of(params, 20)[0]), vec![2, 2]);
        assert_eq!((int_of(params, 50), int_of(params, 60)), (Some(1), None));
    }

    /// Test: the spec decodes to the networks layers with their parameters and a connected graph.
    #[test]
    fn the_spec_decodes_to_the_networks_layers_with_their_parameters_and_a_connected_graph() {
        let n = net(32, 4, 2, 3);
        let bytes = spec(&n, 32).unwrap();
        let model = fields(&bytes);
        assert_eq!(int_of(&model, 1), Some(5), "the specification version");
        // the input and the output, with the input's shape
        let description = fields(bytes_of(&model, 2)[0]);
        let (input, output) = (fields(bytes_of(&description, 1)[0]), fields(bytes_of(&description, 10)[0]));
        assert_eq!(text(bytes_of(&input, 1)[0]), INPUT);
        assert_eq!(text(bytes_of(&output, 1)[0]), OUTPUT);
        let array = fields(bytes_of(&fields(bytes_of(&input, 3)[0]), 5)[0]);
        assert_eq!(packed(bytes_of(&array, 1)[0]), vec![1, 4, 32, 32]);
        assert_eq!(int_of(&array, 2), Some(65568), "32-bit floats at the boundary");
        // the layers, in the order the network's operations make them
        let network = fields(bytes_of(&model, 500)[0]);
        let layers: Vec<Vec<(u32, V)>> = bytes_of(&network, 1).into_iter().map(fields).collect();
        assert_eq!(layers.iter().map(|l| kind(l)).collect::<Vec<_>>(), expected_kinds(&n));
        check_graph(&layers);
        // the parameters of each convolution and pooling layer, against the operation that made it
        let mut convs = n.ops().iter().filter_map(conv_dims);
        for l in &layers {
            let params = fields(bytes_of(l, kind(l))[0]);
            match kind(l) {
                100 => check_conv(&params, convs.next().unwrap()),
                120 => check_pool(&params),
                // a leaky ReLU: the activation's field 15 holds the slope
                130 => assert_eq!(bytes_of(&params, 15).len(), 1),
                320 => assert!(params.is_empty()),
                345 => assert_eq!((int_of(&params, 1), int_of(&params, 2)), (Some(2), Some(2)), "pixel shuffle, block 2"),
                other => panic!("layer kind {other}"),
            }
        }
        assert!(convs.next().is_none(), "every convolution was written");
    }

    /// Test: weights in the coarse end of 16 bit range are counted.
    #[test]
    fn weights_in_the_coarse_end_of_16_bit_range_are_counted() {
        let n = net(32, 4, 1, 3);
        let r = fp16_report(&n);
        assert!(r.nonzero > 0 && r.subnormal + r.lost <= r.nonzero);
        // a network whose weights are all tiny is mostly subnormal; scaling them up brings them back
        let tiny = |f: f32| {
            let ops = n
                .ops()
                .iter()
                .map(|op| match op {
                    Op::Conv(c) => {
                        Op::Conv(Conv { weight: c.weight.iter().map(|w| w * f).collect(), bias: c.bias.iter().map(|b| b * f).collect(), ..c.clone() })
                    }
                    Op::ConvT2(c) => Op::ConvT2(ConvT2 {
                        weight: c.weight.iter().map(|w| w * f).collect(),
                        bias: c.bias.iter().map(|b| b * f).collect(),
                        ..c.clone()
                    }),
                    other => other.clone(),
                })
                .collect();
            Net::new(n.in_channels, ops).unwrap()
        };
        assert!(fp16_report(&tiny(1e-5)).subnormal_share() > 0.5);
        assert!(fp16_report(&tiny(1e-12)).lost > 0);
        assert!(fp16_report(&tiny(1.0)).subnormal_share() < 0.5);
        assert_eq!(Fp16Report::default().subnormal_share(), 0.0);
    }

    /// Test: a small leaky slope asks for more headroom because the value before it can be larger.
    #[test]
    fn a_small_leaky_slope_asks_for_more_headroom_because_the_value_before_it_can_be_larger() {
        let n = net(32, 4, 1, 3);
        let with_alpha = |alpha: Option<f32>| {
            let ops = n
                .ops()
                .iter()
                .map(|op| match op {
                    Op::Conv(c) => Op::Conv(Conv { leaky: alpha.filter(|_| c.leaky.is_some()), ..c.clone() }),
                    Op::ConvT2(c) => Op::ConvT2(ConvT2 { leaky: alpha.filter(|_| c.leaky.is_some()), ..c.clone() }),
                    other => other.clone(),
                })
                .collect();
            Net::new(n.in_channels, ops).unwrap()
        };
        // the same measured peak at the first layer, which has an activation: the scale that layer's bias took is the
        // factor its output was brought down by
        let mut peaks = vec![1.0; n.ops().len() + 1];
        peaks.iter_mut().skip(1).for_each(|p| *p = 20_000.0);
        let first_layer_scale = |alpha: Option<f32>| {
            let (scaled, _) = rescale(&with_alpha(alpha), &peaks).unwrap();
            let (Op::Conv(before), Op::Conv(after)) = (&n.ops()[0], &scaled.ops()[0]) else { panic!("a convolution first") };
            let i = before.bias.iter().position(|b| b.abs() > 1e-6).unwrap();
            after.bias[i] / before.bias[i]
        };
        let (none, mild, steep) = (first_layer_scale(None), first_layer_scale(Some(0.2)), first_layer_scale(Some(0.01)));
        assert!(mild <= none && steep < mild, "a steeper activation gets more room: {none} {mild} {steep}");
        // a slope of zero (a plain ReLU) cannot be bounded: it is treated as 1/64
        assert_eq!(first_layer_scale(Some(0.0)), first_layer_scale(Some(1.0 / 64.0)));
    }
}

/// The written model through Core ML itself (macOS; the Neural Engine where there is one).
#[cfg(test)]
#[cfg(all(target_os = "macos", feature = "runtime"))]
mod core_ml_tests {
    use super::*;
    use crate::cpu::NetRunner;
    use crate::run::TileRunner;
    use crate::{onnx, runtime, synthetic};
    use lightcraft_coreml::{ComputeUnits, Model};
    use std::path::{Path, PathBuf};

    /// A fresh empty folder for a test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lc-denoise-coreml-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write `built`, compile it and load it for `units`.
    fn load(built: &Built, dir: &Path, units: ComputeUnits) -> Model {
        let file = dir.join("net.mlmodel");
        std::fs::write(&file, &built.mlmodel).unwrap();
        let compiled = lightcraft_coreml::compile(&file, dir).unwrap();
        Model::load(&compiled, units, INPUT, OUTPUT).unwrap()
    }

    /// The model's answer in the network's own scale, and how far it is from the CPU's of the largest value.
    fn compare(model: &Model, built: &Built, runner: &NetRunner, tile: usize, input: &[f32]) -> f32 {
        let got = model.predict(&[1, 4, tile, tile], input).unwrap();
        assert_eq!(got.shape, vec![1, 3, 2 * tile, 2 * tile]);
        let want = runner.run(input).unwrap();
        assert_eq!(got.data.len(), want.len());
        assert!(got.data.iter().all(|v| v.is_finite()), "numbers that are not finite");
        let top = want.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-12);
        got.data.iter().zip(&want).fold(0f32, |m, (g, w)| m.max((g / built.output_scale - w).abs())) / top
    }

    /// Test: a synthetic network runs through core ml on every kind of units.
    #[test]
    fn a_synthetic_network_runs_through_core_ml_on_every_kind_of_units() {
        let path = std::env::temp_dir().join(format!("lc-coreml-e2e-{}.onnx", std::process::id()));
        std::fs::write(&path, synthetic::unet_onnx(64, 8, 2, 11)).unwrap();
        let net = onnx::read(&path).unwrap();
        let _ = std::fs::remove_file(path);
        let runner = NetRunner::new(&net, 64).unwrap();
        let (input, _) = runtime::test_tile(64);
        let peaks = runner.peaks(&runtime::check_tile(64)).unwrap();
        let built = build(&net, 64, &peaks).unwrap();
        let dir = scratch("synthetic");
        for units in [ComputeUnits::CpuOnly, ComputeUnits::CpuAndNeuralEngine] {
            let worst = compare(&load(&built, &dir, units), &built, &runner, 64, &input);
            println!("{units:?}: off by {worst:.2e} of the largest value");
            assert!(worst < 2e-2, "{units:?} is off by {worst:e}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Opt-in: `LC_DENOISE_MODEL=<model_bayer.onnx> cargo test -p lightcraft-denoise --features runtime --release
    /// coreml::core_ml_tests::the_real_model -- --nocapture`: the real model's file, on a tile of 512.
    #[test]
    fn the_real_model_runs_through_core_ml() {
        let Some(model_path) = std::env::var_os("LC_DENOISE_MODEL") else {
            lightcraft_testkit::skip!(Model, "set LC_DENOISE_MODEL to the denoise model (.onnx)");
            return;
        };
        let net = onnx::read(Path::new(&model_path)).unwrap();
        let tile = 512;
        let runner = NetRunner::new(&net, tile).unwrap();
        // the peaks come from the tile with the whole range (black to clipped white); the answer is judged on another
        let peaks = runner.peaks(&runtime::check_tile(tile)).unwrap();
        println!("largest activation: {:.3e} (the input's {:.3})", peaks.iter().fold(0f32, |m, p| m.max(*p)), peaks[0]);
        let started = std::time::Instant::now();
        let built = build(&net, tile, &peaks).unwrap();
        let share = |r: &Fp16Report| format!("{} of {} below the normal 16-bit range ({} lost)", r.subnormal + r.lost, r.nonzero, r.lost);
        println!("weights and biases before: {}; after the rescale: {}", share(&built.before), share(&built.after));
        println!(
            "model file: {:.1} MB in {:.0} ms, output scale 2^{}",
            built.mlmodel.len() as f64 / 1e6,
            started.elapsed().as_secs_f64() * 1e3,
            built.output_scale.log2()
        );
        // `LC_COREML_WRITE=<file>`: keep the model file, to look at it with other tools
        if let Some(file) = std::env::var_os("LC_COREML_WRITE") {
            std::fs::write(file, &built.mlmodel).unwrap();
        }
        let dir = scratch("real");
        let started = std::time::Instant::now();
        let model = load(&built, &dir, ComputeUnits::CpuAndNeuralEngine);
        println!("compiled and loaded in {:.2} s", started.elapsed().as_secs_f64());
        let (input, _) = runtime::test_tile(tile);
        let worst = compare(&model, &built, &runner, tile, &input);
        let mut best = f64::INFINITY;
        for _ in 0..5 {
            let t = std::time::Instant::now();
            model.predict(&[1, 4, tile, tile], &input).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
        }
        println!("Neural Engine: {best:.1} ms per tile, off by {worst:.2e} of the largest value");
        assert!(worst < 2e-2);
        // `LC_COREML_LOOP_SECS=25`: keep the Neural Engine busy that long, to read its power with `powermetrics`
        if let Some(secs) = std::env::var("LC_COREML_LOOP_SECS").ok().and_then(|v| v.parse::<f64>().ok()) {
            let (started, mut tiles) = (std::time::Instant::now(), 0u32);
            while started.elapsed().as_secs_f64() < secs.clamp(0.0, 600.0) {
                model.predict(&[1, 4, tile, tile], &input).unwrap();
                tiles += 1;
            }
            println!("{tiles} tiles in {:.1} s", started.elapsed().as_secs_f64());
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
