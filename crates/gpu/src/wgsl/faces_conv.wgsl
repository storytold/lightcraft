// Convolution for the face networks (lightcraft-faces), planar (NCHW) float32, batch 1, any stride, padding and grouping.
//
// `tiled` is an implicit matrix product per group: rows are the group's output channels (M), columns its output
// pixels (N), and K runs over input channel x kernel tap. The input is gathered (im2col) straight into workgroup
// memory, so the lowered matrix is never made. 256 threads make a 64 x 64 block, 4 x 4 values each.
// `direct` gives one output value per thread, for depthwise convolutions (one output channel per group), where a
// 64-row tile would be almost all waste.
//
//   out[co][p] = bias[co] + sum_k W[co][k] * X[k][p]     X[k][p] = input[g*cg + k/taps][oy*sh + ky - pt][ox*sw + kx - pl]

struct P {
    m: u32,      // output channels per group
    n: u32,      // output pixels (oh * ow)
    k: u32,      // cg * kh * kw
    cg: u32,     // input channels per group
    h: u32,
    w: u32,
    ow: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    pt: u32,
    pl: u32,
    bias: u32,   // 1 when there is a bias
    total: u32,  // cout * n
    row: u32,    // workgroups along x, for `direct` and `reduce` (so a large layer can use y too)
    splits: u32, // K is cut into this many slices (z = group * splits + slice), summed by `reduce`
    kchunk: u32, // K values per slice, a multiple of BK
    _a: u32,
    _b: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> wgt: array<f32>;
@group(0) @binding(3) var<storage, read> bias: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<f32>;

const BM: u32 = 64u;
const BN: u32 = 64u;
const BK: u32 = 16u;

var<workgroup> As: array<f32, 1024>; // [BK][BM]
var<workgroup> Bs: array<f32, 1024>; // [BK][BN]

fn gather(g: u32, k: u32, pix: u32) -> f32 {
    let taps = p.kh * p.kw;
    let ci = g * p.cg + k / taps;
    let tap = k % taps;
    let oy = pix / p.ow;
    let ox = pix % p.ow;
    let iy = i32(oy * p.sh + tap / p.kw) - i32(p.pt);
    let ix = i32(ox * p.sw + tap % p.kw) - i32(p.pl);
    if (iy < 0 || ix < 0 || iy >= i32(p.h) || ix >= i32(p.w)) {
        return 0.0;
    }
    return src[(ci * p.h + u32(iy)) * p.w + u32(ix)];
}

@compute @workgroup_size(256)
fn tiled(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let n0 = wg.x * BN;
    let m0 = wg.y * BM;
    let g = wg.z / p.splits;
    let slice = wg.z % p.splits;
    let k_lo = slice * p.kchunk;
    let k_hi = min(p.k, k_lo + p.kchunk);
    let tx = tid % 16u;
    let ty = tid / 16u;
    var acc: array<array<f32, 4>, 4>;
    for (var i = 0u; i < 4u; i++) {
        for (var j = 0u; j < 4u; j++) {
            acc[i][j] = 0.0;
        }
    }
    for (var k0 = k_lo; k0 < k_hi; k0 += BK) {
        // 1024 values of each tile, 4 per thread; every slot has exactly one writer
        for (var e = tid; e < 1024u; e += 256u) {
            // weights: neighbouring threads read neighbouring k of one row
            let mm = e / BK;
            let ka = k0 + e % BK;
            let m = m0 + mm;
            var a = 0.0;
            if (ka < k_hi && m < p.m) {
                a = wgt[(g * p.m + m) * p.k + ka];
            }
            As[(e % BK) * BM + mm] = a;
            // input: neighbouring threads read neighbouring pixels
            let kk = e / BN;
            let k = k0 + kk;
            let pix = n0 + e % BN;
            var b = 0.0;
            if (k < k_hi && pix < p.n) {
                b = gather(g, k, pix);
            }
            Bs[e] = b;
        }
        workgroupBarrier();
        for (var kk = 0u; kk < BK; kk++) {
            var a: array<f32, 4>;
            var b: array<f32, 4>;
            for (var i = 0u; i < 4u; i++) {
                a[i] = As[kk * BM + ty * 4u + i];
                b[i] = Bs[kk * BN + tx + 16u * i];
            }
            for (var i = 0u; i < 4u; i++) {
                for (var j = 0u; j < 4u; j++) {
                    acc[i][j] += a[i] * b[j];
                }
            }
        }
        workgroupBarrier();
    }
    for (var i = 0u; i < 4u; i++) {
        let m = m0 + ty * 4u + i;
        if (m >= p.m) {
            continue;
        }
        let co = g * p.m + m;
        var b0 = 0.0;
        if (p.bias != 0u && p.splits == 1u) {
            b0 = bias[co];
        }
        // with slices, each writes a partial sum of its own: [slice][cout][n]
        let base = slice * p.total + co * p.n;
        for (var j = 0u; j < 4u; j++) {
            // columns tx, tx + 16, tx + 32, tx + 48: neighbouring threads write neighbouring pixels
            let pix = n0 + tx + 16u * j;
            if (pix < p.n) {
                dst[base + pix] = acc[i][j] + b0;
            }
        }
    }
}

@compute @workgroup_size(256)
fn direct(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let at = (wg.y * p.row + wg.x) * 256u + tid;
    if (at >= p.total) {
        return;
    }
    let co = at / p.n;
    let pix = at % p.n;
    let g = co / p.m;
    var v = 0.0;
    if (p.bias != 0u) {
        v = bias[co];
    }
    for (var k = 0u; k < p.k; k++) {
        v += wgt[co * p.k + k] * gather(g, k, pix);
    }
    dst[at] = v;
}

// The sum of the K slices' partial results, plus the bias. `src` holds [splits][cout][n].
@compute @workgroup_size(256)
fn reduce(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let at = (wg.y * p.row + wg.x) * 256u + tid;
    if (at >= p.total) {
        return;
    }
    var v = 0.0;
    if (p.bias != 0u) {
        v = bias[at / p.n];
    }
    for (var s = 0u; s < p.splits; s++) {
        v += src[s * p.total + at];
    }
    dst[at] = v;
}
