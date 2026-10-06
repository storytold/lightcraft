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
