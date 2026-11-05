// Self-attention of the SAM 3 backbone (`sam3.rs`), flash style: the scores are never stored. Each query keeps a
// running maximum, softmax denominator and weighted sum of values while the keys stream past (online softmax), so a
// global layer (5184 tokens, 16 heads) needs no 16 x 5184² score matrix.
//
//   qkv  [tokens, stride]: per token q | k | v, each `hidden` wide (heads x 64), q and k already rotated (RoPE)
//        and q already scaled by 1 / sqrt(64)
//   out  [tokens, hidden]
//
// Tokens are in window order, so a query's keys are the `span` tokens of its window: a contiguous range (span = all
// tokens for the global layers). A workgroup is 64 queries of one head (span is a multiple of 64, so they share
// their window), one query per thread with its q and its partial output in registers; keys and values pass through
// workgroup memory KB at a time, read by all 64 threads at once (a broadcast). The host writes out the per-thread
// work with one variable per vector and score (the DECL, SCORES, RESCALE, ACCUMULATE and STORE marks): arrays indexed
// in loops can end up in slow local memory.

struct P {
    tokens: u32,
    span: u32,
    stride: u32,
    hidden: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> qkv: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> out: array<vec4<f32>>;

// head_dim / 4, keys per step
const D4: u32 = 16u;
const KB: u32 = {{KB}}u;

var<workgroup> Ks: array<vec4<f32>, KB * D4>;
var<workgroup> Vs: array<vec4<f32>, KB * D4>;

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let head = wg.y;
    let qw = wg.x * 64u;
    let tq = qw + lid;
    let s4 = p.stride / 4u;
    let h4 = p.hidden / 4u;
    let first = qw / p.span * p.span;
    // a whole workgroup returns together (tokens is a multiple of 64): the barriers below stay uniform
    if (qw >= p.tokens) {
        return;
    }
    let qa = tq * s4 + head * D4;

{{DECL}}
    var m = -3.0e38;
    var l = 0.0;

    for (var k0 = 0u; k0 < p.span; k0 += KB) {
        for (var e = lid; e < KB * D4; e += 64u) {
            let t = first + k0 + e / D4;
            let c = head * D4 + e % D4;
            Ks[e] = qkv[t * s4 + h4 + c];
            Vs[e] = qkv[t * s4 + 2u * h4 + c];
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

    let r = 1.0 / l;
    let oa = tq * h4 + head * D4;
{{STORE}}
}
