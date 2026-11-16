// Handing a finished render to the window without a readback (`render::present`).
// Bindings: px (packed RGBA8 output), dst (packed RGBA8, padded rows), count (one atomic word).

// Pixels whose alpha is not 255: work that never ran (the readback's `unwritten`). P[0] = pixels.
@compute @workgroup_size(256)
fn unwritten(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    if ((px[i] >> 24u) != 255u) {
        atomicAdd(&count[0], 1u);
    }
}

// Rows of P[0] pixels (P[1] rows) re-laid at a stride of P[2] pixels (texture copies take rows of a
// multiple of 256 bytes).
@compute @workgroup_size(16, 16)
fn pad_rows(@builtin(global_invocation_id) g: vec3<u32>) {
    if (g.x >= pu(0u) || g.y >= pu(1u)) {
        return;
    }
    dst[g.y * pu(2u) + g.x] = px[g.y * pu(0u) + g.x];
}
