// The A-tile loader of the necks' 3 x 3 convolution (`sam3/neck.rs`, padding 1, stride 1): spliced into
// `sam3_matmul.wgsl` in place of its dense `load_a`, so the convolution is that kernel's product over an im2col
// matrix that is never stored (an implicit GEMM; at 288 x 288 x 256 the stored one would be 764 MB).
//
//   X  [side², cin]  the image, pixels row-major, channels last (cin a multiple of 4)
//   A  [side², 9 · cin]  row m: output pixel (m / side, m % side); column k = tap · cin + c: input channel c of the
//      pixel at tap (ky, kx) = (tap / 3, tap % 3), i.e. (row + ky − 1, col + kx − 1), zero outside the image
//   W  [cout, 9 · cin]  the weights in the same (ky, kx, c) order
//
// A vector of four k is four channels of one pixel, so it is one aligned load from X.

// Vector e of the A tile at k0 (row e / KV); zero outside the image (the padding) and outside A.
fn load_a(m0: u32, k0: u32, e: u32) -> vec4<f32> {
    let m = m0 + e / KV;
    let kq = k0 / 4u + e % KV;
    let k4 = p.k / 4u;
    if (m >= p.m || kq >= k4) {
        return vec4<f32>(0.0);
    }
    let side = p.side;
    let c4 = k4 / 9u;
    let tap = kq / c4;
    let row = i32(m / side) + i32(tap / 3u) - 1;
    let col = i32(m % side) + i32(tap % 3u) - 1;
    if (row < 0 || col < 0 || row >= i32(side) || col >= i32(side)) {
        return vec4<f32>(0.0);
    }
    return x[(u32(row) * side + u32(col)) * c4 + kq % c4];
}
