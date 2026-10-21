//! A small ONNX U-Net built in memory, for tests of the layers that read and run denoise networks (here, in the GPU
//! runner and in the engine): the same layer kinds and wiring as the real denoisers, with deterministic random weights.
//! Nothing here is a model anyone would use.

use crate::synthetic_proto::{len_field, value_info_bytes};

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

fn varint_field(n: u64, v: u64, out: &mut Vec<u8>) {
    varint(n << 3, out);
    varint(v, out);
}

enum A<'a> {
    Int(i64),
    Ints(&'a [i64]),
    Float(f32),
    Str(&'a str),
}

fn node(op: &str, inputs: &[&str], output: &str, attrs: &[(&str, A)]) -> Vec<u8> {
    let mut n = Vec::new();
    for i in inputs {
        len_field(1, i.as_bytes(), &mut n);
    }
    len_field(2, output.as_bytes(), &mut n);
    len_field(4, op.as_bytes(), &mut n);
    for (name, value) in attrs {
        let mut a = Vec::new();
        len_field(1, name.as_bytes(), &mut a);
        match value {
            A::Int(v) => {
                varint_field(3, *v as u64, &mut a);
                varint_field(20, 2, &mut a);
            }
            A::Ints(vs) => {
                for v in *vs {
                    varint_field(8, *v as u64, &mut a);
                }
                varint_field(20, 7, &mut a);
            }
            A::Float(v) => {
                varint((2 << 3) | 5, &mut a);
                a.extend_from_slice(&v.to_le_bytes());
                varint_field(20, 1, &mut a);
            }
            A::Str(s) => {
                len_field(4, s.as_bytes(), &mut a);
                varint_field(20, 3, &mut a);
            }
        }
        len_field(5, &a, &mut n);
    }
    n
}

fn tensor(name: &str, dims: &[u64], values: &[f32]) -> Vec<u8> {
    let mut t = Vec::new();
    for d in dims {
        varint_field(1, *d, &mut t);
    }
    varint_field(2, 1, &mut t);
    len_field(8, name.as_bytes(), &mut t);
    let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    len_field(9, &raw, &mut t);
    t
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

struct Builder {
    nodes: Vec<Vec<u8>>,
    inits: Vec<Vec<u8>>,
    rng: Rng,
    n: usize,
}

impl Builder {
    fn name(&mut self, kind: &str) -> String {
        self.n += 1;
        format!("{kind}{}", self.n)
    }

    /// Random weights scaled so activations keep a sane size through the layers.
    fn weights(&mut self, name: &str, dims: &[u64], fan_in: usize) -> String {
        let count: u64 = dims.iter().product();
        let scale = (3.0 / fan_in as f32).sqrt();
        let values: Vec<f32> = (0..count).map(|_| self.rng.next() * scale).collect();
        self.inits.push(tensor(name, dims, &values));
        name.to_string()
    }

    fn bias(&mut self, name: &str, n: u64) -> String {
        let values: Vec<f32> = (0..n).map(|_| self.rng.next() * 0.1).collect();
        self.inits.push(tensor(name, &[n], &values));
        name.to_string()
    }

    /// `Conv` (+ `LeakyRelu` when `leaky`); gives the name of the tensor that results.
    fn conv(&mut self, src: &str, cin: u64, cout: u64, k: u64, leaky: bool) -> String {
        let out = self.name("conv");
        self.conv_as(src, cin, cout, k, leaky, &out)
    }

    /// `Conv` with explicit output node name; returns the resulting tensor name.
    fn conv_as(&mut self, src: &str, cin: u64, cout: u64, k: u64, leaky: bool, out_name: &str) -> String {
        let (w, b) = (self.name("w"), self.name("b"));
        let w = self.weights(&w, &[cout, cin, k, k], (cin * k * k) as usize);
        let b = self.bias(&b, cout);
        let pad = (k as i64 - 1) / 2;
        self.nodes.push(node(
            "Conv",
            &[src, &w, &b],
            out_name,
            &[("kernel_shape", A::Ints(&[k as i64, k as i64])), ("pads", A::Ints(&[pad; 4])), ("strides", A::Ints(&[1, 1])), ("group", A::Int(1))],
        ));
        if !leaky {
            return out_name.to_string();
        }
        let act = self.name("act");
        self.nodes.push(node("LeakyRelu", &[out_name], &act, &[("alpha", A::Float(0.2))]));
        act
    }

    /// `MaxPool` 2×2 with stride 2.
    fn pool(&mut self, src: &str) -> String {
        let out = self.name("pool");
        self.nodes.push(node(
            "MaxPool",
            &[src],
            &out,
            &[("kernel_shape", A::Ints(&[2, 2])), ("strides", A::Ints(&[2, 2])), ("pads", A::Ints(&[0; 4]))],
        ));
        out
    }

    /// `ConvTranspose` 2×2 with stride 2.
    fn up(&mut self, src: &str, cin: u64, cout: u64) -> String {
        let (w, b, out) = (self.name("uw"), self.name("ub"), self.name("up"));
        let w = self.weights(&w, &[cin, cout, 2, 2], (cin * 4) as usize);
        let b = self.bias(&b, cout);
        self.nodes.push(node(
            "ConvTranspose",
            &[src, &w, &b],
            &out,
            &[("kernel_shape", A::Ints(&[2, 2])), ("strides", A::Ints(&[2, 2])), ("pads", A::Ints(&[0; 4]))],
        ));
        out
    }

    /// `Concat` along channel axis 1.
    fn concat(&mut self, a: &str, b: &str) -> String {
        let out = self.name("cat");
        self.nodes.push(node("Concat", &[a, b], &out, &[("axis", A::Int(1))]));
        out
    }
}

/// A denoiser-shaped U-Net as ONNX bytes: `[1, 4, tile, tile]` → `[1, 3, 2·tile, 2·tile]`, with `depth` poolings
/// (so `tile` must be a multiple of `2^depth`) and `ch` channels on the first level, doubling on each (every count a
/// multiple of 4). Each level is two 3 × 3 convolutions with a leaky ReLU, as in the real networks; the output is a
/// 1 × 1 convolution to 12 channels and depth-to-space.
pub fn unet_onnx(tile: u64, ch: u64, depth: usize, seed: u64) -> Vec<u8> {
    let mut b = Builder { nodes: Vec::new(), inits: Vec::new(), rng: Rng(seed | 1), n: 0 };
    let mut skips: Vec<(String, u64)> = Vec::new();
    let (mut cur, mut c_in, mut c) = ("data".to_string(), 4u64, ch);
    for level in 0..=depth {
        let x = b.conv(&cur, c_in, c, 3, true);
        let x = b.conv(&x, c, c, 3, true);
        if level < depth {
            skips.push((x.clone(), c));
            cur = b.pool(&x);
            c_in = c;
            c *= 2;
        } else {
            cur = x;
        }
    }
    for (skip, sc) in skips.into_iter().rev() {
        let up = b.up(&cur, c, sc);
        let joined = b.concat(&up, &skip);
        let x = b.conv(&joined, 2 * sc, sc, 3, true);
        cur = b.conv(&x, sc, sc, 3, true);
        c = sc;
    }
    let rgb = b.conv(&cur, c, 12, 1, false);
    b.nodes.push(node("DepthToSpace", &[&rgb], "rgb", &[("blocksize", A::Int(2)), ("mode", A::Str("CRD"))]));

    let mut graph = Vec::new();
    for n in &b.nodes {
        len_field(1, n, &mut graph);
    }
    for t in &b.inits {
        len_field(5, t, &mut graph);
    }
    len_field(11, &value_info_bytes("data", 1, &[Ok(1), Ok(4), Ok(tile), Ok(tile)]), &mut graph);
    len_field(12, &value_info_bytes("rgb", 1, &[Ok(1), Ok(3), Ok(2 * tile), Ok(2 * tile)]), &mut graph);
    let mut ops = Vec::new();
    len_field(1, b"", &mut ops);
    varint_field(2, 13, &mut ops);
    let mut model = Vec::new();
    varint_field(1, 8, &mut model);
    len_field(7, &graph, &mut model);
    len_field(8, &ops, &mut model);
    model
}

/// A tiny smoothing network for installing a real CPU model in setup-flow tests.
/// It averages a 3 × 3 neighborhood of the four Bayer planes and returns three identical channels.
pub fn smoothing_onnx() -> Vec<u8> {
    let mut graph = Vec::new();
    len_field(1, &node("Conv", &["data", "weights"], "samples", &[("kernel_shape", A::Ints(&[3, 3])), ("pads", A::Ints(&[1, 1, 1, 1]))]), &mut graph);
    len_field(1, &node("DepthToSpace", &["samples"], "rgb", &[("blocksize", A::Int(2)), ("mode", A::Str("CRD"))]), &mut graph);
    len_field(5, &tensor("weights", &[12, 4, 3, 3], &[1.0 / 36.0; 12 * 4 * 3 * 3]), &mut graph);
    len_field(11, &value_info_bytes("data", 1, &[Ok(1), Ok(4), Ok(64), Ok(64)]), &mut graph);
    len_field(12, &value_info_bytes("rgb", 1, &[Ok(1), Ok(3), Ok(128), Ok(128)]), &mut graph);
    let mut model = Vec::new();
    varint_field(1, 8, &mut model);
    len_field(7, &graph, &mut model);
    let mut ops = Vec::new();
    varint_field(2, 13, &mut ops);
    len_field(8, &ops, &mut model);
    model
}

/// A linear-RGB denoiser U-Net as ONNX bytes: `[1, 3, tile, tile]` → `[1, 3, tile, tile]`, with `depth` poolings
/// (so `tile` must be a multiple of `2^depth`) and `ch` channels on the first level.
/// The output is a 1 × 1 convolution directly to 3 channels without DepthToSpace.
pub fn unet_linear_rgb_onnx(tile: u64, ch: u64, depth: usize, seed: u64) -> Vec<u8> {
    let mut b = Builder { nodes: Vec::new(), inits: Vec::new(), rng: Rng(seed | 1), n: 0 };
    let mut skips: Vec<(String, u64)> = Vec::new();
    let (mut cur, mut c_in, mut c) = ("data".to_string(), 3u64, ch);
    for level in 0..=depth {
        let x = b.conv(&cur, c_in, c, 3, true);
        let x = b.conv(&x, c, c, 3, true);
        if level < depth {
            skips.push((x.clone(), c));
            cur = b.pool(&x);
            c_in = c;
            c *= 2;
        } else {
            cur = x;
        }
    }
    for (skip, sc) in skips.into_iter().rev() {
        let up = b.up(&cur, c, sc);
        let joined = b.concat(&up, &skip);
        let x = b.conv(&joined, 2 * sc, sc, 3, true);
        cur = b.conv(&x, sc, sc, 3, true);
        c = sc;
    }
    let _ = b.conv_as(&cur, c, 3, 1, false, "rgb");

    let mut graph = Vec::new();
    for n in &b.nodes {
        len_field(1, n, &mut graph);
    }
    for t in &b.inits {
        len_field(5, t, &mut graph);
    }
    len_field(11, &value_info_bytes("data", 1, &[Ok(1), Ok(3), Ok(tile), Ok(tile)]), &mut graph);
    len_field(12, &value_info_bytes("rgb", 1, &[Ok(1), Ok(3), Ok(tile), Ok(tile)]), &mut graph);
    let mut ops = Vec::new();
    len_field(1, b"", &mut ops);
    varint_field(2, 13, &mut ops);
    let mut model = Vec::new();
    varint_field(1, 8, &mut model);
    len_field(7, &graph, &mut model);
    len_field(8, &ops, &mut model);
    model
}

/// A tiny smoothing network for linear RGB: 3 input channels to 3 output channels (3x3 convolution).
pub fn smoothing_linear_rgb_onnx() -> Vec<u8> {
    let mut graph = Vec::new();
    len_field(
        1,
        &node(
            "Conv",
            &["data", "weights"],
            "rgb",
            &[("kernel_shape", A::Ints(&[3, 3])), ("pads", A::Ints(&[1, 1, 1, 1])), ("strides", A::Ints(&[1, 1])), ("group", A::Int(1))],
        ),
        &mut graph,
    );
    len_field(5, &tensor("weights", &[3, 3, 3, 3], &[1.0 / 27.0; 3 * 3 * 3 * 3]), &mut graph);
    len_field(11, &value_info_bytes("data", 1, &[Ok(1), Ok(3), Ok(64), Ok(64)]), &mut graph);
    len_field(12, &value_info_bytes("rgb", 1, &[Ok(1), Ok(3), Ok(64), Ok(64)]), &mut graph);
    let mut model = Vec::new();
    varint_field(1, 8, &mut model);
    len_field(7, &graph, &mut model);
    let mut ops = Vec::new();
    varint_field(2, 13, &mut ops);
    len_field(8, &ops, &mut model);
    model
}
