// The transforms of one group, on the module-scope array `{{A}}` (the shader is built once for the noisy blocks and once
// for the pilot's). The arrays are not passed to functions: FXC (DX12) cannot index an array it is handed by reference
// without unrolling every loop.

var<private> {{A}}: array<f32, 256>;

fn dct_fwd{{S}}(base: u32) {
    var tmp: array<f32, 16>;
    for (var k = 0u; k < N; k = k + 1u) {
        for (var j = 0u; j < N; j = j + 1u) {
            var s = 0.0;
            for (var i = 0u; i < N; i = i + 1u) {
                s = s + dct(k * N + i) * {{A}}[base + i * N + j];
            }
            tmp[k * N + j] = s;
        }
    }
    for (var k = 0u; k < N; k = k + 1u) {
        for (var l = 0u; l < N; l = l + 1u) {
            var s = 0.0;
            for (var j = 0u; j < N; j = j + 1u) {
                s = s + tmp[k * N + j] * dct(l * N + j);
            }
            {{A}}[base + k * N + l] = s;
        }
    }
}

fn dct_inv{{S}}(base: u32) {
    var tmp: array<f32, 16>;
    for (var i = 0u; i < N; i = i + 1u) {
        for (var l = 0u; l < N; l = l + 1u) {
            var s = 0.0;
            for (var k = 0u; k < N; k = k + 1u) {
                s = s + dct(k * N + i) * {{A}}[base + k * N + l];
            }
            tmp[i * N + l] = s;
        }
    }
    for (var i = 0u; i < N; i = i + 1u) {
        for (var j = 0u; j < N; j = j + 1u) {
            var s = 0.0;
            for (var l = 0u; l < N; l = l + 1u) {
                s = s + tmp[i * N + l] * dct(l * N + j);
            }
            {{A}}[base + i * N + j] = s;
        }
    }
}

// The Haar transform across the `m` blocks (m is a power of two), at each of the 16 positions of a block.
fn haar{{S}}(m: u32, forward: bool) {
    if (m < 2u) {
        return;
    }
    var col: array<f32, 16>;
    var tmp: array<f32, 16>;
    for (var k = 0u; k < NN; k = k + 1u) {
        for (var i = 0u; i < m; i = i + 1u) {
            col[i] = {{A}}[i * NN + k];
        }
        if (forward) {
            var len = m;
            while (len > 1u) {
                let half = len / 2u;
                for (var i = 0u; i < half; i = i + 1u) {
                    let a = col[2u * i];
                    let c = col[2u * i + 1u];
                    tmp[i] = (a + c) * INV_SQRT2;
                    tmp[half + i] = (a - c) * INV_SQRT2;
                }
                for (var i = 0u; i < len; i = i + 1u) {
                    col[i] = tmp[i];
                }
                len = half;
            }
        } else {
            var len = 2u;
            while (len <= m) {
                let half = len / 2u;
                for (var i = 0u; i < half; i = i + 1u) {
                    let avg = col[i];
                    let det = col[half + i];
                    tmp[2u * i] = (avg + det) * INV_SQRT2;
                    tmp[2u * i + 1u] = (avg - det) * INV_SQRT2;
                }
                for (var i = 0u; i < len; i = i + 1u) {
                    col[i] = tmp[i];
                }
                len = len * 2u;
            }
        }
        for (var i = 0u; i < m; i = i + 1u) {
            {{A}}[i * NN + k] = col[i];
        }
    }
}
