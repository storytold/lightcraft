// Collaborative filtering of one group of one plane: the blocks are stacked, transformed (a 2-D DCT of each block, a
// Haar transform across the stack), shrunk (a hard threshold, or a Wiener filter built from the pilot) and transformed
// back. One invocation per group and plane; the filtered blocks and the group's weight are stored for the aggregation.
// (The transforms come from bm3d_transform.wgsl: `buf` is the noisy group, `pb` the pilot's.)

@group(0) @binding(2) var<storage, read> y: array<f32>;
@group(0) @binding(3) var<storage, read> pilot: array<f32>;
@group(0) @binding(4) var<storage, read> members: array<u32>;
@group(0) @binding(5) var<storage, read_write> blocks: array<f32>;
@group(0) @binding(6) var<storage, read_write> weights: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let g = gid.x;
    if (g >= p.nr * p.nr * 4u) {
        return;
    }
    let rf = g / 4u;
    let ch = g % 4u;
    let t = p.t;
    let m = members[rf * STRIDE];
    for (var i = 0u; i < m; i = i + 1u) {
        let pos = members[rf * STRIDE + 1u + i];
        for (var j = 0u; j < N; j = j + 1u) {
            for (var c = 0u; c < N; c = c + 1u) {
                buf[i * NN + j * N + c] = y[ch * t * t + pos + j * t + c];
            }
        }
    }
    for (var i = 0u; i < m; i = i + 1u) {
        dct_fwd(i * NN);
    }
    haar(m, true);
    var weight = 1.0;
    if (p.wiener == 0u) {
        var kept = 0u;
        for (var k = 0u; k < m * NN; k = k + 1u) {
            if (abs(buf[k]) < p.lambda) {
                buf[k] = 0.0;
            } else {
                kept = kept + 1u;
            }
        }
        weight = 1.0 / f32(max(kept, 1u));
    } else {
        for (var i = 0u; i < m; i = i + 1u) {
            let pos = members[rf * STRIDE + 1u + i];
            for (var j = 0u; j < N; j = j + 1u) {
                for (var c = 0u; c < N; c = c + 1u) {
                    pb[i * NN + j * N + c] = pilot[ch * t * t + pos + j * t + c];
                }
            }
        }
        for (var i = 0u; i < m; i = i + 1u) {
            dct_fwd_p(i * NN);
        }
        haar_p(m, true);
        var energy = 0.0;
        for (var k = 0u; k < m * NN; k = k + 1u) {
            let q = pb[k];
            let w = q * q / (q * q + p.sigma * p.sigma);
            buf[k] = buf[k] * w;
            energy = energy + w * w;
        }
        weight = 1.0 / max(energy, 1e-3);
    }
    haar(m, false);
    for (var i = 0u; i < m; i = i + 1u) {
        dct_inv(i * NN);
    }
    let base = (rf * 4u + ch) * K * NN;
    for (var k = 0u; k < m * NN; k = k + 1u) {
        blocks[base + k] = buf[k];
    }
    weights[rf * 4u + ch] = weight;
}
