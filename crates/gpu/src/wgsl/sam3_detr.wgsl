// The DETR encoder of SAM 3's text detector (`sam3/detr.rs`): attention with heads of 32 (8 of them over 256 numbers),
// for the image tokens over themselves (self-attention) and over the prompt's tokens (cross-attention, with a
// key-padding bias), and the sum of two arrays (the position encoding added to the normed tokens).
//
//   q    [queries, q_stride]: head h's q at h · 32 (binding 1)
//   k    [keys, k_stride] from k_off (binding 2), v [keys, v_stride] from v_off (binding 3); 2 and 3 may be one buffer
//   bias `keys` floats in binding 2 from `bias` (NONE: no bias), added to every score of its key: 0 for a real
//        token, f32::MIN for padding, as candle's `padding_bias`
//   out  [queries, heads · 32]
//
// Strides and offsets count floats (multiples of 4); keys is a multiple of KB. Flash style like
// `sam3_attention.wgsl`: one query per thread with its q and partial output in registers, keys and values KB at a
// time through workgroup memory, online softmax. The running maximum starts at f32::MIN rather than below it: a padded
// key's score is then exactly f32::MIN (as in candle, where s + f32::MIN rounds to f32::MIN) and weighs nothing next
// to a real one, and a query whose keys are all padding averages them, as candle's softmax over equal scores does.
// The host writes out the per-thread work with one variable per vector and score (the DECL, SCORES, RESCALE,
// ACCUMULATE and STORE marks).

struct P {
    queries: u32,
    keys: u32,
    q_stride: u32,
    k_stride: u32,
    v_stride: u32,
    k_off: u32,
    v_off: u32,
    bias: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> qs: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read> ks: array<vec4<f32>>;
@group(0) @binding(3) var<storage, read> vs: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> out: array<vec4<f32>>;

// head_dim / 4, keys per step, 1 / sqrt(head_dim)
const D4: u32 = 8u;
const KB: u32 = {{KB}}u;
const SCALE: f32 = 0.17677669529663687;
const NONE: u32 = 0xffffffffu;
const LOWEST: f32 = -3.4028234663852886e38;

var<workgroup> Ks: array<vec4<f32>, KB * D4>;
var<workgroup> Vs: array<vec4<f32>, KB * D4>;
var<workgroup> Bs: array<f32, KB>;

@compute @workgroup_size(64)
fn attention(@builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let head = wg.y;
    let tq = wg.x * 64u + lid;
    // threads past the last query compute on query 0 (the barriers below stay uniform) and store nothing
    let live = tq < p.queries;
    let qa = select(0u, tq, live) * (p.q_stride / 4u) + head * D4;
    let k4 = p.k_stride / 4u;
    let v4 = p.v_stride / 4u;
    let ka = p.k_off / 4u + head * D4;
    let va = p.v_off / 4u + head * D4;

{{DECL}}
    var m = LOWEST;
    var l = 0.0;

    for (var k0 = 0u; k0 < p.keys; k0 += KB) {
        for (var e = lid; e < KB * D4; e += 64u) {
            let t = k0 + e / D4;
            Ks[e] = ks[t * k4 + ka + e % D4];
            Vs[e] = vs[t * v4 + va + e % D4];
        }
        if (lid < KB) {
            var b = 0.0;
            if (p.bias != NONE) {
                let i = p.bias + k0 + lid;
                b = ks[i / 4u][i % 4u];
            }
            Bs[lid] = b;
        }
        workgroupBarrier();

        var top = m;
{{SCORES}}
        // rescale what was summed under the old maximum
        let fix = exp(m - top);
        l *= fix;
{{RESCALE}}
{{ACCUMULATE}}
        m = top;
        workgroupBarrier();
    }

    if (live) {
        let r = 1.0 / l;
        let oa = tq * groups.y * D4 + head * D4;
{{STORE}}
    }
}

// out = qs + ks, `queries` vectors (keys: workgroups per row of the dispatch).
@compute @workgroup_size(256)
fn add(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = (wg.y * p.keys + wg.x) * 256u + lid;
    if (i >= p.queries) {
        return;
    }
    out[i] = qs[i] + ks[i];
}
