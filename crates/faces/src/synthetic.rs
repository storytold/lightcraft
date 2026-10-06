//! A minimal, valid ONNX model made in memory: for tests, and later for the install self-check. It is not a
//! working network (it has no layers), only a graph with the right inputs, outputs and a weight blob.

pub(crate) fn varint(mut v: u64, out: &mut Vec<u8>) {
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

pub fn len_field(n: u64, body: &[u8], out: &mut Vec<u8>) {
    varint(n << 3 | 2, out);
    varint(body.len() as u64, out);
    out.extend_from_slice(body);
}

pub(crate) fn varint_field(n: u64, v: u64, out: &mut Vec<u8>) {
    varint(n << 3, out);
    varint(v, out);
}

/// A graph input or output: `Ok(n)` is a fixed dimension, `Err(name)` a named (dynamic) one.
pub fn value_info_bytes(name: &str, elem: u64, dims: &[std::result::Result<u64, &str>]) -> Vec<u8> {
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

/// A small valid model shaped like a face embedder: float input `data` `[N, 3, 112, 112]`, an old-style
/// weight also listed as an input, a weight with bulk data, float output `emb` `[N, output_dim]`, opset 17.
pub fn embedder_model(output_dim: u64) -> Vec<u8> {
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

/// A node attribute for [`node_bytes`].
enum Attribute<'a> {
    Int(i64),
    Ints(&'a [i64]),
}

fn node_bytes(op: &str, inputs: &[&str], output: &str, attrs: &[(&str, Attribute)]) -> Vec<u8> {
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
            Attribute::Int(v) => {
                varint_field(3, *v as u64, &mut a);
                varint_field(20, 2, &mut a); // INT
            }
            Attribute::Ints(values) => {
                for v in *values {
                    varint_field(8, *v as u64, &mut a);
                }
                varint_field(20, 7, &mut a); // INTS
            }
        }
        len_field(5, &a, &mut n);
    }
    n
}

fn float_tensor_bytes(name: &str, dims: &[u64], values: &[f32]) -> Vec<u8> {
    let mut t = Vec::new();
    for d in dims {
        varint_field(1, *d, &mut t);
    }
    varint_field(2, 1, &mut t); // FLOAT
    len_field(8, name.as_bytes(), &mut t);
    let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    len_field(9, &raw, &mut t);
    t
}

/// A tiny working network shaped like a face embedder, for tests of the runtime: input `data`
/// `[1, 3, 112, 112]`, a 1 × 1 convolution to `dim` channels, global average pooling and flattening to
/// `[1, dim]`. Its output depends on the picture's per-channel brightness, which is all the tests need.
pub fn tiny_embedder_model(dim: u64) -> Vec<u8> {
    let mut graph = Vec::new();
    len_field(1, &node_bytes("Conv", &["data", "w", "b"], "c", &[("kernel_shape", Attribute::Ints(&[1, 1]))]), &mut graph);
    len_field(1, &node_bytes("GlobalAveragePool", &["c"], "p", &[]), &mut graph);
    len_field(1, &node_bytes("Flatten", &["p"], "emb", &[("axis", Attribute::Int(1))]), &mut graph);
    let w: Vec<f32> = (0..dim * 3).map(|i| ((i * 37 % 19) as f32 - 9.0) / 20.0).collect();
    let b: Vec<f32> = (0..dim).map(|i| (i % 5) as f32 / 10.0 - 0.2).collect();
    len_field(5, &float_tensor_bytes("w", &[dim, 3, 1, 1], &w), &mut graph);
    len_field(5, &float_tensor_bytes("b", &[dim], &b), &mut graph);
    len_field(11, &value_info_bytes("data", 1, &[Ok(1), Ok(3), Ok(112), Ok(112)]), &mut graph);
    len_field(12, &value_info_bytes("emb", 1, &[Ok(1), Ok(dim)]), &mut graph);
    let mut ops = Vec::new();
    len_field(1, b"", &mut ops);
    varint_field(2, 13, &mut ops);
    let mut model = Vec::new();
    varint_field(1, 8, &mut model);
    len_field(7, &graph, &mut model);
    len_field(8, &ops, &mut model);
    model
}
