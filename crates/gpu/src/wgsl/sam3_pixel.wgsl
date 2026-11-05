// The small steps of the text detector's pixel decoder (`sam3/pixel.rs`) around its 3 x 3 convolutions: nearest
// up-sampling plus the neck level it is added to, and GroupNorm + ReLU (statistics per chunk of pixels, merged per
// group, then applied in place).
//
// Images are pixel rows (`[pixels, channels]`, row-major); the neck levels come channels-first, as uploaded. Same
// bindings and parameter block as `sam3_ops.wgsl`; flat dispatches cover `count` items, 256 per workgroup, workgroups
// in rows of `row`.

struct P {
    a: u32,
    b: u32,
    c: u32,
    d: u32,
    e: u32,
    f: u32,
    count: u32,
    row: u32,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> src: array<f32>;
@group(0) @binding(2) var<storage, read> aux: array<f32>;
@group(0) @binding(3) var<storage, read> aux2: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<f32>;

fn flat_id(wg: vec3<u32>, lid: u32) -> u32 {
    return (wg.y * p.row + wg.x) * 256u + lid;
}

// Nearest up-sampling of the rows `src` (a: channels, c: side) to side b, plus the channels-first image `aux`
// (`[a, b, b]`): dst[row · b + col, ch] = src[(row · c / b) · c + col · c / b, ch] + aux[ch, row, col]. (candle's
// `upsample_nearest2d` takes floor(i · c / b), the same for the decoder's exact doublings.)
@compute @workgroup_size(256)
fn upsample_add(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    let ch = g % p.a;
    let pixel = g / p.a;
    let row = pixel / p.b;
    let col = pixel % p.b;
    let at = row * p.c / p.b * p.c + col * p.c / p.b;
    dst[g] = src[at * p.a + ch] + aux[ch * p.b * p.b + pixel];
}

var<workgroup> red: array<f32, 256>;

fn reduce(lid: u32, v: f32) -> f32 {
    red[lid] = v;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s >>= 1u) {
        if (lid < s) {
            red[lid] += red[lid + s];
        }
        workgroupBarrier();
    }
    let total = red[0];
    workgroupBarrier();
    return total;
}

// Thread `lid` holds channel lid's `v`: leaves the sum over each group's channels in red[group] (a: channels,
// d: groups).
fn group_sums(lid: u32, v: f32) {
    red[lid] = v;
    workgroupBarrier();
    let per = p.a / p.d;
    var s = 0.0;
    if (lid < p.d) {
        for (var i = 0u; i < per; i++) {
            s += red[lid * per + i];
        }
    }
    workgroupBarrier();
    if (lid < p.d) {
        red[lid] = s;
    }
    workgroupBarrier();
}

// GroupNorm statistics of one chunk of pixels (a: channels ≤ 256, b: pixels, c: pixels per chunk, d: groups), one
// workgroup per chunk, thread = channel: each group's mean over the chunk and the sum of squared differences from
// it, at dst[(chunk · d + group) · 2 + 0 | 1]. Two passes over the chunk (the second from the cache), so a large mean
// costs no precision.
@compute @workgroup_size(256)
fn group_stats(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let chunk = wg.y * p.row + wg.x;
    let first = chunk * p.c;
    if (first >= p.b) {
        return;
    }
    let last = min(first + p.c, p.b);
    let live = lid < p.a;
    let ch = min(lid, p.a - 1u);
    let per = p.a / p.d;
    let n = f32((last - first) * per);
    var s = 0.0;
    if (live) {
        for (var px = first; px < last; px++) {
            s += src[px * p.a + ch];
        }
    }
    group_sums(lid, s);
    let mean = red[ch / per] / n;
    let own = red[min(lid, p.d - 1u)] / n;
    workgroupBarrier();
    var q = 0.0;
    if (live) {
        for (var px = first; px < last; px++) {
            let d = src[px * p.a + ch] - mean;
            q += d * d;
        }
    }
    group_sums(lid, q);
    if (lid < p.d) {
        let o = (chunk * p.d + lid) * 2u;
        dst[o] = own;
        dst[o + 1u] = red[lid];
    }
}

// The chunks' statistics merged, one workgroup per group (a: chunks, b: pixels, c: pixels per chunk, d: groups,
// e: eps bits, f: channels per group): dst[group · 2 + 0 | 1] = the group's mean and 1 / sqrt(variance + eps), the
// variance biased (over all of the group's numbers), as candle's GroupNorm.
@compute @workgroup_size(256)
fn group_merge(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = wg.x;
    let total = f32(p.b * p.f);
    var s = 0.0;
    for (var i = lid; i < p.a; i += 256u) {
        let n = f32(min(p.c, p.b - i * p.c) * p.f);
        s += n * src[(i * p.d + g) * 2u];
    }
    let mean = reduce(lid, s) / total;
    var q = 0.0;
    for (var i = lid; i < p.a; i += 256u) {
        let n = f32(min(p.c, p.b - i * p.c) * p.f);
        let o = (i * p.d + g) * 2u;
        let d = src[o] - mean;
        q += src[o + 1u] + n * d * d;
    }
    let variance = reduce(lid, q) / total;
    if (lid == 0u) {
        dst[g * 2u] = mean;
        dst[g * 2u + 1u] = inverseSqrt(variance + bitcast<f32>(p.e));
    }
}

// GroupNorm and ReLU in place on the rows `dst` (a: channels, d: groups) with the merged statistics `src`, the scale
// `aux` and the shift `aux2` (`[a]` each): dst = max((dst − mean) · rstd · aux + aux2, 0).
@compute @workgroup_size(256)
fn group_norm_relu(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    let ch = g % p.a;
    let group = ch / (p.a / p.d);
    dst[g] = max((dst[g] - src[group * 2u]) * src[group * 2u + 1u] * aux[ch] + aux2[ch], 0.0);
}
