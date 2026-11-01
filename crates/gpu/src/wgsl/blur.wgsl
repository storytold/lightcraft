// Box passes (clamped edges) over an image of NC interleaved channels; three of each approximate a
// Gaussian (`lightcraft_raster::blur::gaussian`).
// P: w, h, nc, r, ch (box_v only). Bindings: src, dst.

// Pixel `i` (all nc ≤ 3 channels at once: one contiguous read per pixel).
fn ld(i: u32, nc: u32) -> vec3<f32> {
    let j = i * nc;
    if (nc == 1u) {
        return vec3<f32>(src[j], 0.0, 0.0);
    }
    if (nc == 2u) {
        return vec3<f32>(src[j], src[j + 1u], 0.0);
    }
    return vec3<f32>(src[j], src[j + 1u], src[j + 2u]);
}

fn st(i: u32, nc: u32, v: vec3<f32>) {
    let j = i * nc;
    dst[j] = v.x;
    if (nc > 1u) {
        dst[j + 1u] = v.y;
    }
    if (nc > 2u) {
        dst[j + 2u] = v.z;
    }
}

// Horizontal: one thread per pixel, summing its 2r + 1 window; adjacent threads read adjacent
// pixels, which the cache serves. (Threads that each slid a running sum over 16 pixels read pixels
// 16 apart across a workgroup: on a Radeon 780M that pass took 9–15× as long as `box_v`.)
@compute @workgroup_size(64, 4)
fn box_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = pu(0u);
    let h = pu(1u);
    let nc = pu(2u);
    let r = i32(pu(3u));
    let x = g.x;
    let y = g.y;
    if (x >= w || y >= h) {
        return;
    }
    let last = i32(w) - 1;
    let row = y * w;
    var acc = vec3<f32>(0.0);
    for (var k = -r; k <= r; k++) {
        acc += ld(row + u32(clamp(i32(x) + k, 0, last)), nc);
    }
    st(row + x, nc, acc * (1.0 / f32(2 * r + 1)));
}

// Vertical: one thread per CH consecutive rows of a column (the CPU re-primes every BAND rows the
// same way).
@compute @workgroup_size(64, 4)
fn box_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = pu(0u);
    let h = pu(1u);
    let nc = pu(2u);
    let r = i32(pu(3u));
    let ch = pu(4u);
    let x = g.x;
    let y0 = g.y * ch;
    if (x >= w || y0 >= h) {
        return;
    }
    let last = i32(h) - 1;
    let inv = 1.0 / f32(2 * r + 1);
    let y1 = min(y0 + ch, h);
    var acc = vec3<f32>(0.0);
    for (var k = -r; k <= r; k++) {
        acc += ld(u32(clamp(i32(y0) + k, 0, last)) * w + x, nc);
    }
    for (var y = y0; y < y1; y++) {
        st(y * w + x, nc, acc * inv);
        acc = acc + ld(u32(min(i32(y) + r + 1, last)) * w + x, nc) - ld(u32(max(i32(y) - r, 0)) * w + x, nc);
    }
}

// Separable median of a single-channel plane (`lightcraft_pipeline::local::median_hv`): 2r+1
// taps (1 ≤ r ≤ 24, clamped edges) along a row (median_h) or a column (median_v). The median is a
// selection, so the result matches the CPU's bit for bit.
// P: w, h, nc (unused), r.
//
// Each thread finds the medians of 4 neighbouring pixels along the pass by rank: a tap is the
// median of a window when fewer than r+1 of its taps are smaller and at least r+1 are smaller or
// equal. The 4 windows share all but their first and last 3 taps, so one count over the shared
// taps serves all four. A 16×16 workgroup (64 pixels along the pass, 16 lines across) first loads
// its taps into `med_tile`. On a 6960×4640 plane at r = 10 this takes 47 ms on an M2 Max, against
// 191 ms for a partial selection sort per pixel over a private array of taps, which drivers keep in
// scratch memory because it is indexed dynamically.
const MED_K: u32 = 4u;
const MED_LINE: u32 = 113u; // 16·MED_K + 2·24, odd
// Not zero-initialised (`build_jobs` in ctx.rs turns that off): write every element read.
var<workgroup> med_tile: array<f32, 1808>; // 16 lines

// The medians of the 2r+1-tap windows starting at `med_tile[base + k]`, k < 4 (r ≥ 1). Taps 3 to
// n-1 are in all four windows; the three at each end are in some.
fn median4(base: u32, r: u32) -> vec4<f32> {
    let n = 2u * r + 1u;
    let e0 = vec3<f32>(med_tile[base], med_tile[base + 1u], med_tile[base + 2u]);
    let e1 = vec3<f32>(med_tile[base + n], med_tile[base + n + 1u], med_tile[base + n + 2u]);
    var out = vec4<f32>(0.0);
    var found = vec4<bool>(false);
    for (var i = 0u; i < n + 3u; i++) {
        let c = med_tile[base + i];
        var less = 0u;
        var le = 0u;
        for (var j = 3u; j < n; j++) {
            let v = med_tile[base + j];
            less += select(0u, 1u, v < c);
            le += select(0u, 1u, v <= c);
        }
        let l0 = select(vec3<u32>(0u), vec3<u32>(1u), e0 < vec3<f32>(c));
        let l1 = select(vec3<u32>(0u), vec3<u32>(1u), e1 < vec3<f32>(c));
        let q0 = select(vec3<u32>(0u), vec3<u32>(1u), e0 <= vec3<f32>(c));
        let q1 = select(vec3<u32>(0u), vec3<u32>(1u), e1 <= vec3<f32>(c));
        // window k: taps k..k+n-1 = taps k..2 of the start, the middle, taps n..n+k-1 of the end
        let lw = vec4<u32>(l0.x + l0.y + l0.z, l0.y + l0.z + l1.x, l0.z + l1.x + l1.y, l1.x + l1.y + l1.z) + vec4<u32>(less);
        let qw = vec4<u32>(q0.x + q0.y + q0.z, q0.y + q0.z + q1.x, q0.z + q1.x + q1.y, q1.x + q1.y + q1.z) + vec4<u32>(le);
        let has = vec4<bool>((i < n), (i >= 1u) && (i < n + 1u), (i >= 2u) && (i < n + 2u), (i >= 3u));
        let hit = has & !found & (lw <= vec4<u32>(r)) & (qw > vec4<u32>(r));
        out = select(out, vec4<f32>(c), hit);
        found = found | hit;
        if (all(found)) {
            break;
        }
    }
    return out;
}

// Thread (l.x, l.y) of a workgroup takes line l.x and pixels 4·l.y to 4·l.y + 3 along it, so the
// threads of a SIMD group read 16 different lines at once; with an odd line length those fall in
// different banks of workgroup memory.
@compute @workgroup_size(16, 16)
fn median_h(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) wg: vec3<u32>) {
    let w = pu(0u);
    let h = pu(1u);
    let r = min(pu(3u), 24u);
    let span = 16u * MED_K + 2u * r;
    let x0 = i32(wg.x * 16u * MED_K) - i32(r);
    // load: consecutive threads read consecutive pixels of a row
    for (var e = l.y * 16u + l.x; e < 16u * span; e += 256u) {
        let line = e / span;
        let k = e % span;
        let y = min(wg.y * 16u + line, h - 1u);
        let x = clamp(x0 + i32(k), 0, i32(w) - 1);
        med_tile[line * MED_LINE + k] = src[y * w + u32(x)];
    }
    workgroupBarrier();
    let y = wg.y * 16u + l.x;
    let x = (wg.x * 16u + l.y) * MED_K;
    if (x >= w || y >= h) {
        return;
    }
    let m = median4(l.x * MED_LINE + l.y * MED_K, r);
    for (var k = 0u; k < MED_K; k++) {
        if (x + k < w) {
            dst[y * w + x + k] = m[k];
        }
    }
}

@compute @workgroup_size(16, 16)
fn median_v(@builtin(local_invocation_id) l: vec3<u32>, @builtin(workgroup_id) wg: vec3<u32>) {
    let w = pu(0u);
    let h = pu(1u);
    let r = min(pu(3u), 24u);
    let span = 16u * MED_K + 2u * r;
    let y0 = i32(wg.y * 16u * MED_K) - i32(r);
    // load: consecutive threads read consecutive pixels of a row (16 columns)
    for (var e = l.y * 16u + l.x; e < 16u * span; e += 256u) {
        let line = e % 16u;
        let k = e / 16u;
        let x = min(wg.x * 16u + line, w - 1u);
        let y = clamp(y0 + i32(k), 0, i32(h) - 1);
        med_tile[line * MED_LINE + k] = src[u32(y) * w + x];
    }
    workgroupBarrier();
    let x = wg.x * 16u + l.x;
    let y = (wg.y * 16u + l.y) * MED_K;
    if (x >= w || y >= h) {
        return;
    }
    let m = median4(l.x * MED_LINE + l.y * MED_K, r);
    for (var k = 0u; k < MED_K; k++) {
        if (y + k < h) {
            dst[(y + k) * w + x] = m[k];
        }
    }
}
