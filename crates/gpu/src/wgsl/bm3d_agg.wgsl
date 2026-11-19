// Putting the filtered blocks back: every cell becomes the weighted average of the filtered blocks that cover it, added
// in the order the reference adds them (groups in raster order, a group's blocks in order), so no two invocations
// write the same cell. One invocation per cell, all four planes.

@group(0) @binding(2) var<storage, read> y: array<f32>;
@group(0) @binding(3) var<storage, read> members: array<u32>;
@group(0) @binding(4) var<storage, read> blocks: array<f32>;
@group(0) @binding(5) var<storage, read> weights: array<f32>;
@group(0) @binding(6) var<storage, read_write> dst: array<f32>;

// The first group start that is at least `v`.
fn lower_bound(v: u32) -> u32 {
    var lo = 0u;
    var hi = p.nr;
    while (lo < hi) {
        let mid = (lo + hi) / 2u;
        if (axis(mid) < v) {
            lo = mid + 1u;
        } else {
            hi = mid;
        }
    }
    return lo;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let g = gid.x;
    let t = p.t;
    if (g >= t * t) {
        return;
    }
    let cx = g % t;
    let cy = g / t;
    // a group's blocks lie within `search` of its start and are N wide: only these starts can reach this cell
    let reach = p.search + N - 1u;
    var xlo = 0u;
    if (cx >= reach) {
        xlo = cx - reach;
    }
    var ylo = 0u;
    if (cy >= reach) {
        ylo = cy - reach;
    }
    var num: array<f32, 4>;
    var den: array<f32, 4>;
    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        num[ch] = 0.0;
        den[ch] = 0.0;
    }
    for (var rj = lower_bound(ylo); rj < p.nr; rj = rj + 1u) {
        if (axis(rj) > cy + p.search) {
            break;
        }
        for (var ri = lower_bound(xlo); ri < p.nr; ri = ri + 1u) {
            if (axis(ri) > cx + p.search) {
                break;
            }
            let rf = rj * p.nr + ri;
            let m = members[rf * STRIDE];
            for (var i = 0u; i < m; i = i + 1u) {
                let pos = members[rf * STRIDE + 1u + i];
                let px = pos % t;
                let py = pos / t;
                if (px <= cx && cx < px + N && py <= cy && cy < py + N) {
                    let cell = (cy - py) * N + (cx - px);
                    let w = win(cell);
                    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
                        let ww = weights[rf * 4u + ch] * w;
                        num[ch] = num[ch] + ww * blocks[((rf * 4u + ch) * K + i) * NN + cell];
                        den[ch] = den[ch] + ww;
                    }
                }
            }
        }
    }
    // cells no block reached keep their value
    for (var ch = 0u; ch < 4u; ch = ch + 1u) {
        let own = y[ch * t * t + g];
        if (den[ch] > 0.0) {
            dst[ch * t * t + g] = num[ch] / den[ch];
        } else {
            dst[ch * t * t + g] = own;
        }
    }
}
