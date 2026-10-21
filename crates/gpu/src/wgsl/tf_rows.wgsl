// Row-wise kernels for the content models' transformers, one workgroup of 256 threads per row (rows = wg.y * gx + wg.x):
//
//   norm:     dst[r] = (src[r] - mean) / sqrt(var + eps) * w + b                  (layer norm over `cols`)
//   softmax:  buf[r] = softmax(buf[r]) in place; with `causal`, row r is query i = r % lq and sees keys 0..=i only
//
// Means and variances are two passes over the row (as the CPU path), reduced in workgroup memory.

struct P {
    rows: u32,
    cols: u32,
    gx: u32,
    lq: u32,
    causal: u32,
    eps: f32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> nw: array<f32>;
@group(0) @binding(3) var<storage, read> nb: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<f32>;

var<workgroup> red: array<f32, 256>;

fn reduce_sum(tid: u32, v: f32) -> f32 {
    red[tid] = v;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s >>= 1u) {
        if (tid < s) {
            red[tid] += red[tid + s];
        }
        workgroupBarrier();
    }
    let r = red[0];
    workgroupBarrier();
    return r;
}

fn reduce_max(tid: u32, v: f32) -> f32 {
    red[tid] = v;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s >>= 1u) {
        if (tid < s) {
            red[tid] = max(red[tid], red[tid + s]);
        }
        workgroupBarrier();
    }
    let r = red[0];
    workgroupBarrier();
    return r;
}

@compute @workgroup_size(256)
fn norm(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let row = wg.y * p.gx + wg.x;
    if (row >= p.rows) {
        return;
    }
    let base = row * p.cols;
    var s = 0.0;
    for (var i = tid; i < p.cols; i += 256u) {
        s += src[base + i];
    }
    let mean = reduce_sum(tid, s) / f32(p.cols);
    var q = 0.0;
    for (var i = tid; i < p.cols; i += 256u) {
        let d = src[base + i] - mean;
        q += d * d;
    }
    let inv = 1.0 / sqrt(reduce_sum(tid, q) / f32(p.cols) + p.eps);
    for (var i = tid; i < p.cols; i += 256u) {
        dst[base + i] = (src[base + i] - mean) * inv * nw[i] + nb[i];
    }
}

@compute @workgroup_size(256)
fn softmax(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let row = wg.y * p.gx + wg.x;
    if (row >= p.rows) {
        return;
    }
    let base = row * p.cols;
    var seen = p.cols;
    if (p.causal != 0u) {
        seen = min(row % p.lq + 1u, p.cols);
    }
    var mx = -3.0e38;
    for (var i = tid; i < seen; i += 256u) {
        mx = max(mx, dst[base + i]);
    }
    let m = reduce_max(tid, mx);
    var s = 0.0;
    for (var i = tid; i < seen; i += 256u) {
        s += exp(dst[base + i] - m);
    }
    let inv = 1.0 / reduce_sum(tid, s);
    for (var i = tid; i < p.cols; i += 256u) {
        var v = 0.0;
        if (i < seen) {
            v = exp(dst[base + i] - m) * inv;
        }
        dst[base + i] = v;
    }
}
