// The histogram (`Histogram::of_srgb8`) of a region of the packed RGBA8 output, on the same
// sampling grid: every STEP-th pixel of every STEP-th row. Bindings: px (packed RGBA8), bins
// (r | g | b | luma, 256 each, then the total; cleared beforehand).
// P: W (image width), X0, Y0 (region origin), NX, NY (sampled columns / rows), STEP.

@compute @workgroup_size(16, 16)
fn histogram(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= pu(3u) || g.y >= pu(4u)) {
        return;
    }
    let step = pu(5u);
    let x = pu(1u) + g.x * step;
    let y = pu(2u) + g.y * step;
    let p = px[y * pu(0u) + x];
    let r = p & 255u;
    let gg = (p >> 8u) & 255u;
    let b = (p >> 16u) & 255u;
    atomicAdd(&bins[r], 1u);
    atomicAdd(&bins[256u + gg], 1u);
    atomicAdd(&bins[512u + b], 1u);
    let l = min((r * 54u + gg * 183u + b * 19u) >> 8u, 255u);
    atomicAdd(&bins[768u + l], 1u);
    atomicAdd(&bins[1024u], 1u);
}
