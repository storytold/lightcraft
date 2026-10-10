// Exact separable Gaussian with explicit taps over a one-channel plane (clamped edges); mirrors
// `lightcraft_raster::blur::gaussian_taps` (same taps, same accumulation order).
// P: w, h, r (taps has 2r+1 entries). Bindings: src, taps, dst.

@compute @workgroup_size(16, 16)
fn gauss_h(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = pu(0u);
    let h = pu(1u);
    let r = i32(pu(2u));
    if (g.x >= w || g.y >= h) {
        return;
    }
    let last = i32(w) - 1;
    let row = g.y * w;
    var acc = 0.0;
    for (var k = 0; k <= 2 * r; k++) {
        acc += src[row + u32(clamp(i32(g.x) + k - r, 0, last))] * taps[u32(k)];
    }
    dst[row + g.x] = acc;
}

@compute @workgroup_size(16, 16)
fn gauss_v(@builtin(global_invocation_id) g: vec3<u32>) {
    let w = pu(0u);
    let h = pu(1u);
    let r = i32(pu(2u));
    if (g.x >= w || g.y >= h) {
        return;
    }
    let last = i32(h) - 1;
    var acc = 0.0;
    for (var k = 0; k <= 2 * r; k++) {
        acc += src[u32(clamp(i32(g.y) + k - r, 0, last)) * w + g.x] * taps[u32(k)];
    }
    dst[g.y * w + g.x] = acc;
}
