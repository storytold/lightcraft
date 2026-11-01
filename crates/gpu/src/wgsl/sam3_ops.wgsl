// The small steps of the SAM 3 backbone (`sam3.rs`) around its matrix products and attention: layer norm, rotary
// position encoding, the patch embedding's im2col and the final reordering to channels-first.
//
// Tokens are kept in window order: token t = window · win² + (row % win) · win + col % win, windows row-major over
// the grid (win = 24 on the 72 x 72 grid). The per-token steps don't care; `im2col` puts the patches in this order
// and `to_chw` takes them out of it.
//
// One uniform block of up to eight numbers per step (`P`); flat dispatches cover `count` items, 256 per workgroup,
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

// The position in window order of grid cell (row, col).
fn window_token(row: u32, col: u32, grid: u32, win: u32) -> u32 {
    let w = row / win * (grid / win) + col / win;
    return w * win * win + row % win * win + col % win;
}

var<workgroup> red: array<f32, 256>;

fn reduce(lid: u32, v: f32) -> f32 {
    red[lid] = v;
    workgroupBarrier();
    for (var s = 128u; s > 0u; s >>= 1u) {
        if (lid < s) {
            red[lid] += red[lid + s];
        }
        workgroupBarrier();
    }
    let total = red[0];
    workgroupBarrier();
    return total;
}

// LayerNorm over rows of `c` numbers (a: rows, b: c, e: eps bits): dst = (src − mean) / sqrt(var + eps) · aux + aux2.
// One workgroup per row.
@compute @workgroup_size(256)
fn layernorm(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let row = wg.y * p.row + wg.x;
    if (row >= p.a) {
        return;
    }
    let c = p.b;
    let base = row * c;
    var s = 0.0;
    for (var i = lid; i < c; i += 256u) {
        s += src[base + i];
    }
    let mean = reduce(lid, s) / f32(c);
    var q = 0.0;
    for (var i = lid; i < c; i += 256u) {
        let d = src[base + i] - mean;
        q += d * d;
    }
    let rstd = inverseSqrt(reduce(lid, q) / f32(c) + bitcast<f32>(p.e));
    for (var i = lid; i < c; i += 256u) {
        dst[base + i] = (src[base + i] - mean) * rstd * aux[i] + aux2[i];
    }
}

// Rotary position encoding of q and k in place, and q scaled (a: tokens, b: row stride, c: hidden = heads · 64,
// d: rows of the table, f: q scale bits). One thread per pair (x, y) of neighbouring numbers of a head:
// (x cos − y sin, y cos + x sin), cos and sin from row t % d of `aux` ([rows, 32] pairs of cos, sin).
@compute @workgroup_size(256)
fn rope(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    let half = p.c / 2u;
    let t = g / p.c;
    let r = g % p.c;
    let which = r / half;
    let pair = r % half;
    let i = t * p.b + which * p.c + 2u * pair;
    let at = ((t % p.d) * 32u + pair % 32u) * 2u;
    let cs = aux[at];
    let sn = aux[at + 1u];
    let x = dst[i];
    let y = dst[i + 1u];
    let scale = select(1.0, bitcast<f32>(p.f), which == 0u);
    dst[i] = (x * cs - y * sn) * scale;
    dst[i + 1u] = (y * cs + x * sn) * scale;
}

// The patches as rows, in window order (a: image side, b: patch, c: grid side, d: window, e: channels):
// dst[t, (ch · patch + ky) · patch + kx] = src[ch, row · patch + ky, col · patch + kx] for the cell (row, col) of t.
@compute @workgroup_size(256)
fn im2col(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    let side = p.a;
    let size = p.b;
    let grid = p.c;
    let k = p.e * size * size;
    let t = g / k;
    let r = g % k;
    let ch = r / (size * size);
    let ky = r / size % size;
    let kx = r % size;
    // the cell of window-order token t
    let win = p.d;
    let w = t / (win * win);
    let l = t % (win * win);
    let row = w / (grid / win) * win + l / win;
    let col = w % (grid / win) * win + l % win;
    dst[g] = src[(ch * side + row * size + ky) * side + col * size + kx];
}

// Tokens in window order to channels-first (a: channels, b: grid side, c: window): dst[ch, row, col] = src[t, ch].
@compute @workgroup_size(256)
fn to_chw(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let g = flat_id(wg, lid);
    if (g >= p.count) {
        return;
    }
    let n = p.b * p.b;
    let ch = g / n;
    let cell = g % n;
    let t = window_token(cell / p.b, cell % p.b, p.b, p.c);
    dst[g] = src[t * p.a + ch];
}
