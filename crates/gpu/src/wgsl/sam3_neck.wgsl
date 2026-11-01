// The reorderings of the SAM 3 necks (`sam3/neck.rs`) around their products: the backbone's channels-first output
// to pixel rows, and the rows of the transposed convolutions back to row-major. (Back to channels-first: `to_chw`
// of `sam3_ops.wgsl`, with one window over the whole image.)
//
// Same bindings and parameter block as `sam3_ops.wgsl`; flat dispatches cover `count` items, 256 per workgroup,
// workgroups in rows of `row`.

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

// Channels-first to pixel rows (a: channels, b: pixels): dst[pixel, ch] = src[ch, pixel].
@compute @workgroup_size(256)
fn to_rows(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    dst[g] = src[g % p.a * p.b + g / p.a];
}

// Pixel rows after `c` 2 x 2 stride-2 transposed convolutions to row-major (a: channels, b: side). Such a convolution
// is one product per input pixel whose row holds its four output pixels (ky, kx) one after the other, so after c of
// them the pixels of the side x side image come ordered by (row >> c, col >> c), then the (ky, kx) of the first, …,
// of the last: dst[row · side + col, ch] = src[that position, ch].
@compute @workgroup_size(256)
fn unshuffle(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    let side = p.b;
    let ups = p.c;
    let ch = g % p.a;
    let pixel = g / p.a;
    let row = pixel / side;
    let col = pixel % side;
    var at = (row >> ups) * (side >> ups) + (col >> ups);
    for (var s = ups; s > 0u; s--) {
        at = at * 4u + ((row >> (s - 1u)) & 1u) * 2u + ((col >> (s - 1u)) & 1u);
    }
    dst[g] = src[at * p.a + ch];
}
