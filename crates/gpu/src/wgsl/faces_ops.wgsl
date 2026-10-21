// The face networks' other operators (lightcraft-faces `net::program`), so activations stay on the device between layers.
// Same bindings as faces_conv.wgsl: the parameters, three read-only operands and the output. Element kernels run one
// thread per output value; `gavg` and `gemv` one workgroup per output value.
//
// Parameters are 24 numbers: q(0) = values (or workgroups) to make, q(1) = workgroups along x, then per kernel.

struct P {
    v: array<vec4<u32>, 6>,
}

@group(0) @binding(0) var<uniform> p: P;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read> c: array<f32>;
@group(0) @binding(4) var<storage, read_write> dst: array<f32>;

fn q(i: u32) -> u32 {
    return p.v[i / 4u][i % 4u];
}

fn index(wid: vec3<u32>, lid: u32) -> u32 {
    return (wid.y * q(1) + wid.x) * 256u + lid;
}

// q(2): 0 relu, 1 sigmoid
@compute @workgroup_size(256)
fn unary(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    let x = a[i];
    if (q(2) == 0u) {
        dst[i] = max(x, 0.0);
    } else {
        dst[i] = 1.0 / (1.0 + exp(-x));
    }
}

// q(2) plane, q(3) channels: x * b[c] + c[c]
@compute @workgroup_size(256)
fn affine(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    let ch = (i / q(2)) % q(3);
    dst[i] = a[i] * b[ch] + c[ch];
}

// q(2) plane, q(3) channels: x < 0 ? x * b[c] : x
@compute @workgroup_size(256)
fn prelu(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    let x = a[i];
    let ch = (i / q(2)) % q(3);
    dst[i] = select(x, x * b[ch], x < 0.0);
}

// q(2): 0 add, 1 sub, 2 mul; q(3..7) output dims; q(7..11) a's strides; q(11..15) b's strides
@compute @workgroup_size(256)
fn binary(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    var rest = i;
    var ia = 0u;
    var ib = 0u;
    for (var d = 3; d >= 0; d = d - 1) {
        let du = u32(d);
        let size = q(3u + du);
        let at = rest % size;
        rest = rest / size;
        ia = ia + at * q(7u + du);
        ib = ib + at * q(11u + du);
    }
    let x = a[ia];
    let y = b[ib];
    if (q(2) == 0u) {
        dst[i] = x + y;
    } else if (q(2) == 1u) {
        dst[i] = x - y;
    } else {
        dst[i] = x * y;
    }
}

// q(3..7) output dims; q(7..11) the input's strides for each output axis
@compute @workgroup_size(256)
fn transpose(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    var rest = i;
    var at_in = 0u;
    for (var d = 3; d >= 0; d = d - 1) {
        let du = u32(d);
        let size = q(3u + du);
        at_in = at_in + (rest % size) * q(7u + du);
        rest = rest / size;
    }
    dst[i] = a[at_in];
}

// q(2) h, q(3) w, q(4) kh, q(5) kw, q(6) sh, q(7) sw, q(8) oh, q(9) ow
@compute @workgroup_size(256)
fn maxpool(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    let h = q(2);
    let w = q(3);
    let oh = q(8);
    let ow = q(9);
    let ox = i % ow;
    let oy = (i / ow) % oh;
    let ch = i / (ow * oh);
    var m = -3.402823e38;
    for (var ky = 0u; ky < q(4); ky = ky + 1u) {
        for (var kx = 0u; kx < q(5); kx = kx + 1u) {
            let y = oy * q(6) + ky;
            let x = ox * q(7) + kx;
            if (y < h && x < w) {
                m = max(m, a[(ch * h + y) * w + x]);
            }
        }
    }
    dst[i] = m;
}

// q(2) h, q(3) w, q(4) sh, q(5) sw: nearest upscale by whole numbers
@compute @workgroup_size(256)
fn resize(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let i = index(wid, lid);
    if (i >= q(0)) { return; }
    let h = q(2);
    let w = q(3);
    let ow = w * q(5);
    let oh = h * q(4);
    let ox = i % ow;
    let oy = (i / ow) % oh;
    let ch = i / (ow * oh);
    dst[i] = a[(ch * h + oy / q(4)) * w + ox / q(5)];
}

var<workgroup> partial: array<f32, 256>;

fn reduce(lid: u32) {
    for (var s = 128u; s > 0u; s = s >> 1u) {
        workgroupBarrier();
        if (lid < s) {
            partial[lid] = partial[lid] + partial[lid + s];
        }
    }
    workgroupBarrier();
}

// one workgroup per channel; q(2) plane
@compute @workgroup_size(256)
fn gavg(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let ch = wid.y * q(1) + wid.x;
    let plane = q(2);
    var sum = 0.0;
    if (ch < q(0)) {
        for (var j = lid; j < plane; j = j + 256u) {
            sum = sum + a[ch * plane + j];
        }
    }
    partial[lid] = sum;
    reduce(lid);
    if (lid == 0u && ch < q(0)) {
        dst[ch] = partial[0] / f32(plane);
    }
}

// one workgroup per output; q(2) k, q(3) n, q(4) 1 when B is [n][k], q(5) alpha's bits
@compute @workgroup_size(256)
fn gemv(@builtin(workgroup_id) wid: vec3<u32>, @builtin(local_invocation_index) lid: u32) {
    let o = wid.y * q(1) + wid.x;
    let k = q(2);
    let n = q(3);
    var sum = 0.0;
    if (o < q(0)) {
        for (var j = lid; j < k; j = j + 256u) {
            let wi = select(j * n + o, o * k + j, q(4) == 1u);
            sum = sum + a[j] * b[wi];
        }
    }
    partial[lid] = sum;
    reduce(lid);
    if (lid == 0u && o < q(0)) {
        dst[o] = bitcast<f32>(q(5)) * partial[0] + c[o];
    }
}
