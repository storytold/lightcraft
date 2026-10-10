// Convolution for the denoise networks, as an implicit matrix product: every output pixel is a row, every input
// channel of every kernel tap is a column of K, and the output channels are the columns of N.
//
//   out[m][n] = bias[n] + sum_k A[m][k] * W[k][n]        A[m][k] = input pixel (m + tap offset), channel ci
//
// Tensors are NHWC, channels innermost, read four at a time (every channel count is a multiple of 4). A workgroup of
// 256 threads makes a BM x BN block of the output, 4 x 4 values per thread, and walks K in steps of BK through
// workgroup memory. The source can be two tensors whose channels follow each other (a skip connection's join, so it
// is never made), and a leaky ReLU is applied on the way out. Specialised by the host: BM, BN, BK (BM * BN = 4096).
//
// Output modes:
//   0  NHWC at the input's size.
//   1  a 2x2 stride-2 transposed convolution: the columns are (dy, dx, co) and each lands in the output pixel
//      (2y + dy, 2x + dx) of an output twice the size.
//   2  the last 1x1 convolution followed by depth-to-space (CRD): columns 4c + 2dy + dx land in plane c at
//      (2y + dy, 2x + dx) of a planar (CHW) output twice the size.

struct P {
    h: u32,
    w: u32,
    c0: u32,
    c1: u32,
    n: u32,
    npad: u32,
    k: u32,
    mode: u32,
    cout: u32,
    leaky: u32,
    alpha: f32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src0: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> src1: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> wgt: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> bias: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> dst: array<f32>;

const BM: u32 = {{BM}}u;
const BN: u32 = {{BN}}u;
const BK: u32 = {{BK}}u;
const TX: u32 = BN / 4u;
const A_ITEMS: u32 = BM * (BK / 4u);
const B_ITEMS: u32 = BK * (BN / 4u);

// A[k][m] has one scalar per pixel; B[k][n] packs four channels per vec4.
// Different invocations must not write lanes of the same shared vector: WGSL permits
// a component write to access the entire vector. A later barrier cannot undo that race.
// Each scalar below has one writer; gather four pixels only after the barrier.
var<workgroup> As: array<f32, BK * BM>;
var<workgroup> Bs: array<vec4<f32>, {{B_VECS}}>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let m0 = wg.x * BM;
    let n0 = wg.y * BN;
    let tx = tid % TX;
    let ty = tid / TX;
    let hw = p.h * p.w;
    let cin = p.c0 + p.c1;
    let taps = p.k * p.k;
    let pad = (p.k - 1u) / 2u;
    let chunks = cin / BK;

    var acc0 = vec4<f32>(0.0);
    var acc1 = vec4<f32>(0.0);
    var acc2 = vec4<f32>(0.0);
    var acc3 = vec4<f32>(0.0);

    for (var tap = 0u; tap < taps; tap++) {
        let ty_off = i32(tap / p.k) - i32(pad);
        let tx_off = i32(tap % p.k) - i32(pad);
        for (var chunk = 0u; chunk < chunks; chunk++) {
            let ci0 = chunk * BK;
            for (var item = tid; item < A_ITEMS; item += 256u) {
                let ml = item / (BK / 4u);
                let q = item % (BK / 4u);
                let m = m0 + ml;
                var v = vec4<f32>(0.0);
                if (m < hw) {
                    let y = i32(m / p.w) + ty_off;
                    let x = i32(m % p.w) + tx_off;
                    if (y >= 0 && y < i32(p.h) && x >= 0 && x < i32(p.w)) {
                        let pix = u32(y) * p.w + u32(x);
                        let ci = ci0 + q * 4u;
                        if (ci < p.c0) {
                            v = src0[pix * (p.c0 / 4u) + ci / 4u];
                        } else {
                            v = src1[pix * (p.c1 / 4u) + (ci - p.c0) / 4u];
                        }
                    }
                }
                let base = q * 4u * BM + ml;
                As[base] = v.x;
                As[base + BM] = v.y;
                As[base + 2u * BM] = v.z;
                As[base + 3u * BM] = v.w;
            }
            for (var item = tid; item < B_ITEMS; item += 256u) {
                let r = item / (BN / 4u);
                let c4 = item % (BN / 4u);
                let krow = tap * cin + ci0 + r;
                Bs[item] = wgt[krow * (p.npad / 4u) + n0 / 4u + c4];
            }
            workgroupBarrier();
            for (var kk = 0u; kk < BK; kk++) {
                let ai = kk * BM + ty * 4u;
                let a = vec4<f32>(As[ai], As[ai + 1u], As[ai + 2u], As[ai + 3u]);
                let b = Bs[kk * TX + tx];
                acc0 += a.x * b;
                acc1 += a.y * b;
                acc2 += a.z * b;
                acc3 += a.w * b;
            }
            workgroupBarrier();
        }
    }

    let n = n0 + tx * 4u;
    if (n >= p.n) {
        return;
    }
    let bv = bias[n / 4u];
    for (var i = 0u; i < 4u; i++) {
        let m = m0 + ty * 4u + i;
        if (m >= hw) {
            continue;
        }
        var v = bv;
        if (i == 0u) { v += acc0; } else if (i == 1u) { v += acc1; } else if (i == 2u) { v += acc2; } else { v += acc3; }
        if (p.leaky != 0u) {
            v = select(v * p.alpha, v, v >= vec4<f32>(0.0));
        }
        if (p.mode == 0u) {
            let o = m * p.n + n;
            dst[o] = v.x;
            dst[o + 1u] = v.y;
            dst[o + 2u] = v.z;
            dst[o + 3u] = v.w;
        } else if (p.mode == 1u) {
            let q = n / p.cout;
            let co = n % p.cout;
            let y = m / p.w;
            let x = m % p.w;
            let opix = (2u * y + q / 2u) * (2u * p.w) + 2u * x + q % 2u;
            let o = opix * p.cout + co;
            dst[o] = v.x;
            dst[o + 1u] = v.y;
            dst[o + 2u] = v.z;
            dst[o + 3u] = v.w;
        } else {
            let c = n / 4u;
            let y = m / p.w;
            let x = m % p.w;
            let ow = 2u * p.w;
            let plane = c * (2u * p.h) * ow;
            let o = plane + (2u * y) * ow + 2u * x;
            dst[o] = v.x;
            dst[o + 1u] = v.y;
            dst[o + ow] = v.z;
            dst[o + ow + 1u] = v.w;
        }
    }
}
