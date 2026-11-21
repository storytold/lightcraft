// Wavelet luminance noise reduction (`lightcraft_pipeline::nrl`). Each kernel mirrors its CPU twin
// there, in the same order of operations; keep them in step.
//
// Buffers: `a`, `b`, `c` inputs, `dst` output (read-write: the detail is formed in place and the change
// of Y accumulates). Planes are one value per pixel, images RGB interleaved.

// `nrl::mirror`: into 0..n without repeating the edge sample.
fn nmirror(i: i32, n: i32) -> i32 {
    var j = i;
    if (j < 0) {
        j = -j;
    }
    if (j >= n) {
        j = 2 * n - 2 - j;
    }
    return clamp(j, 0, n - 1);
}

const B3_0: f32 = 1.0 / 16.0;
const B3_1: f32 = 4.0 / 16.0;
const B3_2: f32 = 6.0 / 16.0;

// Y of RGB image `a`. P: n.
@compute @workgroup_size(256)
fn nrl_y(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = lum2020(vec3<f32>(a[3u * i], a[3u * i + 1u], a[3u * i + 2u]));
}

// `nrl::smooth`, horizontal half: B3 taps 2^j apart. P: w, h, s.
@compute @workgroup_size(16, 16)
fn nrl_smooth_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(pu(0u));
    let h = i32(pu(1u));
    let s = i32(pu(2u));
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) {
        return;
    }
    let r = y * w;
    let t0 = a[r + nmirror(x - 2 * s, w)];
    let t1 = a[r + nmirror(x - s, w)];
    let t2 = a[r + x];
    let t3 = a[r + nmirror(x + s, w)];
    let t4 = a[r + nmirror(x + 2 * s, w)];
    dst[r + x] = B3_0 * t0 + B3_1 * t1 + B3_2 * t2 + B3_1 * t3 + B3_0 * t4;
}

// `nrl::smooth`, vertical half. P: w, h, s.
@compute @workgroup_size(16, 16)
fn nrl_smooth_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(pu(0u));
    let h = i32(pu(1u));
    let s = i32(pu(2u));
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) {
        return;
    }
    let t0 = a[nmirror(y - 2 * s, h) * w + x];
    let t1 = a[nmirror(y - s, h) * w + x];
    let t2 = a[y * w + x];
    let t3 = a[nmirror(y + s, h) * w + x];
    let t4 = a[nmirror(y + 2 * s, h) * w + x];
    dst[y * w + x] = B3_0 * t0 + B3_1 * t1 + B3_2 * t2 + B3_1 * t3 + B3_0 * t4;
}

// `JointNoise::var(0, c2)`. P: n, alpha, beta, ymax.
@compute @workgroup_size(256)
fn nrl_var(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = max(pf(1u) * max(min(a[i], pf(3u)), 0.0) + pf(2u), 1e-14);
}

// The detail in place of the finer approximation: dst − a. P: n.
@compute @workgroup_size(256)
fn nrl_detail(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = dst[i] - a[i];
}

// `keep` and the removed part (`nrl::shrink_fine` / `shrink_coarse`).
fn nrl_removed(d: f32, v: f32, e: f32, k: f32) -> f32 {
    let keep = max(1.0 - k * v / max(e, 1e-30), 0.0);
    return d * (1.0 - keep);
}

// |d| at (row r, column x) of detail `a`, rows and columns mirrored.
fn nrl_p(r: i32, x: i32, w: i32) -> f32 {
    return abs(a[r * w + nmirror(x, w)]);
}

// The largest |d| of three horizontal neighbours.
fn nrl_ph(r: i32, x: i32, w: i32) -> f32 {
    return max(max(nrl_p(r, x - 1, w), nrl_p(r, x, w)), nrl_p(r, x + 1, w));
}

// The largest |d| of three vertical neighbours of (mirrored) row r.
fn nrl_pv(r: i32, x: i32, w: i32, h: i32) -> f32 {
    return max(max(nrl_p(nmirror(r - 1, h), x, w), nrl_p(r, x, w)), nrl_p(nmirror(r + 1, h), x, w));
}

// `nrl::shrink_fine`: directional energy along eight directions, then the shrinking, into `dst`.
// a = detail, b = var. P: w, h, half (2^j), k, DIRECTIONAL_MEAN_SQUARE.
@compute @workgroup_size(16, 16)
fn nrl_fine(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = i32(pu(0u));
    let h = i32(pu(1u));
    let hs = i32(pu(2u));
    let k = pf(3u);
    let dms = pf(4u);
    let x = i32(g.x);
    let y = i32(g.y);
    if (x >= w || y >= h) {
        return;
    }
    let ss = 2 * hs;
    let rm2 = nmirror(y - 2 * hs, h);
    let rm1 = nmirror(y - hs, h);
    let r0 = y;
    let rp1 = nmirror(y + hs, h);
    let rp2 = nmirror(y + 2 * hs, h);
    var m = min(nrl_p(r0, x + ss, w), nrl_p(r0, x - ss, w));
    m = max(m, min(nrl_p(rp2, x, w), nrl_p(rm2, x, w)));
    m = max(m, min(nrl_p(rp2, x + ss, w), nrl_p(rm2, x - ss, w)));
    m = max(m, min(nrl_p(rm2, x + ss, w), nrl_p(rp2, x - ss, w)));
    m = max(m, min(nrl_ph(rp2, x + hs, w), nrl_ph(rm2, x - hs, w)));
    m = max(m, min(nrl_ph(rp2, x - hs, w), nrl_ph(rm2, x + hs, w)));
    m = max(m, min(nrl_pv(rp1, x + ss, w, h), nrl_pv(rm1, x - ss, w, h)));
    m = max(m, min(nrl_pv(rm1, x + ss, w, h), nrl_pv(rp1, x - ss, w, h)));
    m = max(m, 0.0);
    let e = m * m / dms;
    let i = u32(y * w + x);
    dst[i] = dst[i] - nrl_removed(a[i], b[i], e, k);
}

// d² of detail `a`. P: n.
@compute @workgroup_size(256)
fn nrl_sq(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = a[i] * a[i];
}

// `nrl::shrink_coarse`: a = detail, b = var, c = energy (Gaussian mean of d²). P: n, k.
@compute @workgroup_size(256)
fn nrl_coarse(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = dst[i] - nrl_removed(a[i], b[i], c[i], pf(1u));
}

// `nrl::add_luminance` of RGB image `a` and the change of Y `b`. P: n.
@compute @workgroup_size(256)
fn nrl_apply(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let c = array<f32, 3>(a[3u * i], a[3u * i + 1u], a[3u * i + 2u]);
    let dy = b[i];
    let wt = array<f32, 3>(0.2627, 0.6780, 0.0593);
    var fl = array<f32, 3>(min(c[0], 0.0), min(c[1], 0.0), min(c[2], 0.0));
    var out = c;
    if (c[0] + dy >= fl[0] && c[1] + dy >= fl[1] && c[2] + dy >= fl[2]) {
        out = array<f32, 3>(c[0] + dy, c[1] + dy, c[2] + dy);
    } else {
        var left = dy;
        var free = array<bool, 3>(true, true, true);
        for (var it = 0u; it < 3u; it++) {
            var ws = 0.0;
            for (var k = 0u; k < 3u; k++) {
                if (free[k]) {
                    ws += wt[k];
                }
            }
            if (left >= 0.0 || ws <= 0.0) {
                break;
            }
            let stp = left / ws;
            var taken = 0.0;
            for (var k = 0u; k < 3u; k++) {
                if (free[k]) {
                    let v = max(out[k] + stp, fl[k]);
                    taken += (v - out[k]) * wt[k];
                    free[k] = v > fl[k];
                    out[k] = v;
                }
            }
            left -= taken;
            if (abs(left) <= 1e-12) {
                break;
            }
        }
    }
    dst[3u * i] = out[0];
    dst[3u * i + 1u] = out[1];
    dst[3u * i + 2u] = out[2];
}
