// The linear layers of the SAM 3 backbone (`sam3.rs`): Y = act(X · Wᵀ + bias), or Y += X · Wᵀ + bias (a residual);
// act is none, GELU (gelu = 1) or ReLU (gelu = 2, the DETR encoder's MLP, `sam3/detr.rs`).
//
//   X     [M, K]  row-major activations
//   W     [N, K]  row-major: the checkpoint's own layout (f32; or, with the f16 variant, pairs of halves packed into
//                 u32s, k first)
//   bias  [bias_rows, N]: row m adds bias row m % bias_rows (1: an ordinary bias; 576: the patch embedding's
//                 24 x 24 position table, which repeats over the windows)
//   Y     [M, N]
//
// K is a multiple of 4 (of BK for the f16 variant); M and N are anything. A workgroup of 256 threads (16 x 16) makes a
// BM x BN block of Y, TM x TN values per thread: rows ty + 16 i and columns tx + 16 j, so neighbouring threads write
// neighbouring numbers. K is walked in steps of BK through workgroup memory, where both tiles are kept k-contiguous
// as they are loaded (four k per vector), each row padded by one vector so that the eight threads of a 128-bit load
// phase read eight different bank groups. The next step's tiles are fetched into registers while the current one is
// multiplied. The host writes out the per-thread work with one variable per accumulator (the DECL, LOAD, STORE_TILE,
// COMPUTE and STORE marks): arrays indexed in loops can end up in slow local memory.

struct P {
    m: u32,
    n: u32,
    k: u32,
    bias_rows: u32,
    gelu: u32,
    residual: u32,
    // the image side of the 3 x 3 convolution's loader (`sam3_conv.wgsl`); unused here
    side: u32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> x: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> w: array<{{WEIGHT}}>;
@group(0) @binding(3) var<storage, read> bias: array<f32>;
@group(0) @binding(4) var<storage, read_write> y: array<f32>;

const BM: u32 = {{BM}}u;
const BN: u32 = {{BN}}u;
const BK: u32 = {{BK}}u;
// vectors per tile row, and the padded row stride
const KV: u32 = BK / 4u;
const KS: u32 = KV + 1u;

var<workgroup> As: array<vec4<f32>, BM * KS>;
var<workgroup> Bs: array<vec4<f32>, BN * KS>;

// #if dense
// Vector e of the A tile at k0 (row e / KV); zero outside X.
fn load_a(m0: u32, k0: u32, e: u32) -> vec4<f32> {
    let m = m0 + e / KV;
    let kq = k0 / 4u + e % KV;
    let k4 = p.k / 4u;
    if (m < p.m && kq < k4) {
        return x[m * k4 + kq];
    }
    return vec4<f32>(0.0);
}
// #endif

// #if f32
// Vector e of the B tile at k0 (row e / KV of W); zero outside W.
fn load_b(n0: u32, k0: u32, e: u32) -> vec4<f32> {
    let n = n0 + e / KV;
    let kq = k0 / 4u + e % KV;
    let k4 = p.k / 4u;
    if (n < p.n && kq < k4) {
        return w[n * k4 + kq];
    }
    return vec4<f32>(0.0);
}
// #endif

// #if f16
// The f16 variant: packed vector e of the B tile at k0 (eight k of row e / (BK / 8)), as two f32 vectors.
fn load_b16(n0: u32, k0: u32, e: u32) -> array<vec4<f32>, 2> {
    let n = n0 + e / (BK / 8u);
    let kq = k0 / 8u + e % (BK / 8u);
    let k8 = p.k / 8u;
    var v = vec4<u32>(0u);
    if (n < p.n && kq < k8) {
        v = w[n * k8 + kq];
    }
    return array<vec4<f32>, 2>(vec4<f32>(unpack2x16float(v.x), unpack2x16float(v.y)), vec4<f32>(unpack2x16float(v.z), unpack2x16float(v.w)));
}
// #endif

fn erf(v: f32) -> f32 {
    // Abramowitz & Stegun 7.1.26 (|error| < 1.5e-7)
    let a = abs(v);
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let r = 1.0 - poly * exp(-a * a);
    return select(-r, r, v >= 0.0);
}

fn put(m: u32, n: u32, acc: f32) {
    if (n >= p.n) {
        return;
    }
    var v = acc + bias[(m % p.bias_rows) * p.n + n];
    if (p.gelu == 1u) {
        // GELU, exact (erf) form
        v = 0.5 * v * (1.0 + erf(v * 0.7071067811865476));
    } else if (p.gelu == 2u) {
        // ReLU (the DETR encoder's MLP)
        v = max(v, 0.0);
    }
    let o = m * p.n + n;
    if (p.residual != 0u) {
        v += y[o];
    }
    y[o] = v;
}

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let n0 = wg.x * BN;
    let m0 = wg.y * BM;
    let tx = tid % 16u;
    let ty = tid / 16u;

{{DECL}}

{{LOAD}}
    for (var k0 = 0u; k0 < p.k; k0 += BK) {
{{STORE_TILE}}
        workgroupBarrier();
        if (k0 + BK < p.k) {
{{LOAD_NEXT}}
        }
        for (var kv = 0u; kv < KV; kv++) {
{{COMPUTE}}
        }
        workgroupBarrier();
    }

{{STORE}}
}
