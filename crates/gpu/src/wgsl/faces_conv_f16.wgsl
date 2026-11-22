enable f16;

// Mixed-precision FP16 convolution for the face networks (lcc-faces), planar (NCHW), batched [B, C, H, W].
// Weights, activations and workgroup tiles are stored in IEEE-754 `f16` (halving VRAM and memory bandwidth, and
// doubling shared-memory density), while multiply-add accumulators and K-slice reductions stay in `f32` to preserve
// accuracy across deep networks.

struct P {
    m: u32,
    n: u32,
    k: u32,
    cg: u32,
    h: u32,
    w: u32,
    ow: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    pt: u32,
    pl: u32,
    bias: u32,
    total: u32,
    row: u32,
    splits: u32,
    kchunk: u32,
    plane_out: u32,
    cout: u32,
    cin: u32,
    batch: u32,
    _a: u32,
    _b: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f16>;
@group(0) @binding(2) var<storage, read> wgt: array<f16>;
@group(0) @binding(3) var<storage, read> bias: array<f16>;
@group(0) @binding(4) var<storage, read_write> dst: array<f16>;

const BM: u32 = 64u;
const BN: u32 = 64u;
const BK: u32 = 16u;

var<workgroup> As: array<f16, 1024>; // [BK][BM]
var<workgroup> Bs: array<f16, 1024>; // [BK][BN]

fn gather(g: u32, k: u32, pix: u32) -> f16 {
    let b_idx = pix / p.plane_out;
    let lpix = pix % p.plane_out;
    let taps = p.kh * p.kw;
    let ci = g * p.cg + k / taps;
    let tap = k % taps;
    let oy = lpix / p.ow;
    let ox = lpix % p.ow;
    let iy = i32(oy * p.sh + tap / p.kw) - i32(p.pt);
    let ix = i32(ox * p.sw + tap % p.kw) - i32(p.pl);
    if (iy < 0 || ix < 0 || iy >= i32(p.h) || ix >= i32(p.w)) {
        return 0.0h;
    }
    return src[((b_idx * p.cin + ci) * p.h + u32(iy)) * p.w + u32(ix)];
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
        for (var e = tid; e < 1024u; e += 256u) {
            let mm = e / BK;
            let ka = k0 + e % BK;
            let m = m0 + mm;
            var a = 0.0h;
            if (ka < k_hi && m < p.m) {
                a = wgt[(g * p.m + m) * p.k + ka];
            }
            As[(e % BK) * BM + mm] = a;
            let kk = e / BN;
            let k = k0 + kk;
            let pix = n0 + e % BN;
            var b = 0.0h;
            if (k < k_hi && pix < p.n) {
                b = gather(g, k, pix);
            }
            Bs[e] = b;
        }
        workgroupBarrier();
        for (var kk = 0u; kk < BK; kk++) {
            var a: array<f16, 4>;
            var b: array<f16, 4>;
            for (var i = 0u; i < 4u; i++) {
                a[i] = As[kk * BM + ty * 4u + i];
                b[i] = Bs[kk * BN + tx + 16u * i];
            }
            for (var i = 0u; i < 4u; i++) {
                let af = f32(a[i]);
                for (var j = 0u; j < 4u; j++) {
                    acc[i][j] += af * f32(b[j]);
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
            b0 = f32(bias[co]);
        }
        let slice_base = slice * p.total;
        for (var j = 0u; j < 4u; j++) {
            let pix = n0 + tx + 16u * j;
            if (pix < p.n) {
                let b_idx = pix / p.plane_out;
                let lpix = pix % p.plane_out;
                dst[slice_base + (b_idx * p.cout + co) * p.plane_out + lpix] = f16(acc[i][j] + b0);
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
    let batch_plane = p.cout * p.plane_out;
    let b_idx = at / batch_plane;
    let rem = at % batch_plane;
    let co = rem / p.plane_out;
    let lpix = rem % p.plane_out;
    let g = co / p.m;
    let pix = b_idx * p.plane_out + lpix;
    var v = 0.0;
    if (p.bias != 0u) {
        v = f32(bias[co]);
    }
    for (var k = 0u; k < p.k; k++) {
        v += f32(wgt[co * p.k + k]) * f32(gather(g, k, pix));
    }
    dst[at] = f16(v);
}

@compute @workgroup_size(256)
fn reduce(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let at = (wg.y * p.row + wg.x) * 256u + tid;
    if (at >= p.total) {
        return;
    }
    var v = 0.0;
    if (p.bias != 0u) {
        let co = (at / p.plane_out) % p.cout;
        v = f32(bias[co]);
    }
    for (var s = 0u; s < p.splits; s++) {
        v += f32(src[s * p.total + at]);
    }
    dst[at] = f16(v);
}
