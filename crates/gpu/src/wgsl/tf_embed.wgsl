// Elementwise kernels for the content models' transformers (one thread per output number, i = (wg.y * gx + wg.x) * 256 + t):
//
//   assemble: x[b][t] = (class token when cls and t == 0, else src[b * (len - cls) + t - cls]) + pos[t]
//             (embeddings in front of the first block: patches or token rows, a class token, position embeddings)
//   gather:   x[r] = src[r * len + pick[r]]   (one position per input: a class or end-of-text token)
//
// Rows are `cols` numbers; `total` = rows * cols.

struct P {
    total: u32,
    cols: u32,
    len: u32,
    cls: u32,
    gx: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> class_token: array<f32>;
@group(0) @binding(3) var<storage, read> pos: array<f32>;
@group(0) @binding(4) var<storage, read> pick: array<u32>;
@group(0) @binding(5) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256)
fn assemble(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let i = (wg.y * p.gx + wg.x) * 256u + t;
    if (i >= p.total) {
        return;
    }
    let row = i / p.cols;
    let col = i % p.cols;
    let b = row / p.len;
    let at = row % p.len;
    var v: f32;
    if (p.cls != 0u && at == 0u) {
        v = class_token[col];
    } else {
        v = src[(b * (p.len - p.cls) + at - p.cls) * p.cols + col];
    }
    dst[i] = v + pos[at * p.cols + col];
}

@compute @workgroup_size(256)
fn gather(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) t: u32) {
    let i = (wg.y * p.gx + wg.x) * 256u + t;
    if (i >= p.total) {
        return;
    }
    let row = i / p.cols;
    let col = i % p.cols;
    dst[i] = src[(row * p.len + pick[row]) * p.cols + col];
}
