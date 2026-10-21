// A linear layer for the content models' transformers: Y = act(X · W + bias), or Y += (X · W + bias) (a residual).
//
//   X  [M, K]  row-major (activations)
//   W  [K, N]  row-major (the layer's weight, transposed on upload)
//   Y  [M, N]
//
// K and N are multiples of 4 (checked by the host): everything is read four numbers at a time. A workgroup of 256
// threads (16 x 16) makes a BM x BN block of Y, TM rows x NV vec4 columns per thread (the rows next to each other; the
// vec4 columns 16 apart, so neighbouring threads read neighbouring vec4s of the B tile), walking K in steps of 16
// through workgroup memory. The host writes out the per-thread work with one variable per accumulator (the DECL,
// COMPUTE and STORE marks below): an array indexed in loops can end up in slow local memory.
//
// Activations: 0 none, 1 GELU (tanh approximation), 2 GELU (erf), 3 quick GELU (x * sigmoid(1.702 x)).

struct P {
    m: u32,
    n: u32,
    k: u32,
    act: u32,
    acc: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> x: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> w: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> bias: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> y: array<vec4<f32>>;

const BM: u32 = {{BM}}u;
const BN: u32 = {{BN}}u;
const BK: u32 = 16u;
const TM: u32 = BM / 16u;

// The A tile as [m][k / 4] and the B tile as [k][n / 4]: every vector is written whole by one thread (issue #479).
var<workgroup> As: array<vec4<f32>, BM * BK / 4u>;
var<workgroup> Bs: array<vec4<f32>, BK * BN / 4u>;

fn erf(v: vec4<f32>) -> vec4<f32> {
    // Abramowitz & Stegun 7.1.26 (|error| < 1.5e-7)
    let a = abs(v);
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let r = 1.0 - poly * exp(-a * a);
    return select(-r, r, v >= vec4<f32>(0.0));
}

fn activate(v: vec4<f32>) -> vec4<f32> {
    switch p.act {
        case 1u: {
            // 0.5 x (1 + tanh(sqrt(2 / pi) (x + 0.044715 x^3))); tanh's argument bounded (tanh(10) is 1 in f32)
            let inner = clamp(0.7978845608028654 * (v + 0.044715 * v * v * v), vec4<f32>(-10.0), vec4<f32>(10.0));
            return 0.5 * v * (1.0 + tanh(inner));
        }
        case 2u: {
            return 0.5 * v * (1.0 + erf(v * 0.7071067811865476));
        }
        case 3u: {
            return v / (1.0 + exp(min(-1.702 * v, vec4<f32>(80.0))));
        }
        default: {
            return v;
        }
    }
}

fn put(m: u32, nq: u32, v: vec4<f32>) {
    if (m < p.m && nq < p.n / 4u) {
        var r = activate(v + bias[nq]);
        let o = m * (p.n / 4u) + nq;
        if (p.acc != 0u) {
            r += y[o];
        }
        y[o] = r;
    }
}

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let n0 = wg.x * BN;
    let m0 = wg.y * BM;
    let tx = tid % 16u;
    let ty = tid / 16u;
    let k4 = p.k / 4u;
    let n4 = p.n / 4u;
    let arow = ty * TM;

{{DECL}}

    for (var k0 = 0u; k0 < p.k; k0 += BK) {
        for (var e = tid; e < BM * (BK / 4u); e += 256u) {
            let r = e / (BK / 4u);
            let kq = k0 / 4u + e % (BK / 4u);
            let m = m0 + r;
            var v = vec4<f32>(0.0);
            if (m < p.m && kq < k4) {
                v = x[m * k4 + kq];
            }
            As[e] = v;
        }
        for (var e = tid; e < BK * (BN / 4u); e += 256u) {
            let kk = k0 + e / (BN / 4u);
            let nq = n0 / 4u + e % (BN / 4u);
            var v = vec4<f32>(0.0);
            if (kk < p.k && nq < n4) {
                v = w[kk * n4 + nq];
            }
            Bs[e] = v;
        }
        workgroupBarrier();
        for (var kv = 0u; kv < BK / 4u; kv++) {
{{COMPUTE}}
        }
        workgroupBarrier();
    }

    let mrow = m0 + arow;
    let ncol = n0 / 4u + tx;
{{STORE}}
}
