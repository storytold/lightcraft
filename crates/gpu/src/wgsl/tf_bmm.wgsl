// Batched matrix products for attention: C[z] = alpha * A[z] · B[z], for z = b * heads + h, every operand addressed
// by strides (so the heads are read straight out of the packed q/k/v rows and written back into merged rows):
//
//   A[z][i][k] = a[a_off + b * a_sb + h * a_sh + i * a_si + k * a_sk]
//   B[z][k][j] = b[b_off + b * b_sb + h * b_sh + k * b_sk + j * b_sj]
//   C[z][i][j] = c[c_off + b * c_sb + h * c_sh + i * c_si + j * c_sj]
//
// Q · K^T (scores) and P · V (attention output) are both this. A workgroup of 256 threads makes a 64 x 64 block of
// one z, 4 x 4 values per thread, K in steps of 16 through workgroup memory; `a_kfast` / `b_kfast` say whether k is
// the operand's contiguous index (so the tile loads read neighbouring numbers).

struct P {
    m: u32,
    n: u32,
    k: u32,
    heads: u32,
    a_off: u32,
    a_sb: u32,
    a_sh: u32,
    a_si: u32,
    a_sk: u32,
    b_off: u32,
    b_sb: u32,
    b_sh: u32,
    b_sk: u32,
    b_sj: u32,
    c_off: u32,
    c_sb: u32,
    c_sh: u32,
    c_si: u32,
    c_sj: u32,
    alpha: f32,
    a_kfast: u32,
    b_kfast: u32,
    pad0: u32,
    pad1: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> c: array<f32>;

const T: u32 = 64u;
const BK: u32 = 16u;

var<workgroup> As: array<f32, BK * T>;
var<workgroup> Bs: array<f32, BK * T>;

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) tid: u32) {
    let j0 = wg.x * T;
    let i0 = wg.y * T;
    let z = wg.z;
    let zb = z / p.heads;
    let zh = z % p.heads;
    let abase = p.a_off + zb * p.a_sb + zh * p.a_sh;
    let bbase = p.b_off + zb * p.b_sb + zh * p.b_sh;
    let tx = tid % 16u;
    let ty = tid / 16u;

    var acc: array<vec4<f32>, 4>;
    for (var i = 0u; i < 4u; i++) {
        acc[i] = vec4<f32>(0.0);
    }

    for (var k0 = 0u; k0 < p.k; k0 += BK) {
        for (var e = tid; e < BK * T; e += 256u) {
            var r: u32;
            var kk: u32;
            if (p.a_kfast != 0u) {
                kk = e % BK;
                r = e / BK;
            } else {
                r = e % T;
                kk = e / T;
            }
            let i = i0 + r;
            let k = k0 + kk;
            var v = 0.0;
            if (i < p.m && k < p.k) {
                v = a[abase + i * p.a_si + k * p.a_sk];
            }
            As[kk * T + r] = v;
        }
        for (var e = tid; e < BK * T; e += 256u) {
            var cc: u32;
            var kk: u32;
            if (p.b_kfast != 0u) {
                kk = e % BK;
                cc = e / BK;
            } else {
                cc = e % T;
                kk = e / T;
            }
            let j = j0 + cc;
            let k = k0 + kk;
            var v = 0.0;
            if (j < p.n && k < p.k) {
                v = b[bbase + k * p.b_sk + j * p.b_sj];
            }
            Bs[kk * T + cc] = v;
        }
        workgroupBarrier();
        for (var kk = 0u; kk < BK; kk++) {
            let av = vec4<f32>(As[kk * T + ty * 4u], As[kk * T + ty * 4u + 1u], As[kk * T + ty * 4u + 2u], As[kk * T + ty * 4u + 3u]);
            let bv = vec4<f32>(Bs[kk * T + tx], Bs[kk * T + tx + 16u], Bs[kk * T + tx + 32u], Bs[kk * T + tx + 48u]);
            acc[0] = fma(vec4<f32>(av.x), bv, acc[0]);
            acc[1] = fma(vec4<f32>(av.y), bv, acc[1]);
            acc[2] = fma(vec4<f32>(av.z), bv, acc[2]);
            acc[3] = fma(vec4<f32>(av.w), bv, acc[3]);
        }
        workgroupBarrier();
    }

    let cbase = p.c_off + zb * p.c_sb + zh * p.c_sh;
    for (var r = 0u; r < 4u; r++) {
        let i = i0 + ty * 4u + r;
        if (i >= p.m) {
            break;
        }
        let v = acc[r] * p.alpha;
        for (var q = 0u; q < 4u; q++) {
            let j = j0 + tx + q * 16u;
            if (j < p.n) {
                c[cbase + i * p.c_si + j * p.c_sj] = v[q];
            }
        }
    }
}
