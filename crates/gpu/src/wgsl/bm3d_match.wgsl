// Grouping: for every block that starts a group, its nearest blocks (itself first) within the search window, nearest
// first, at most `cap` and at most `tau` away, cut to a power of two. One invocation per group.

@group(0) @binding(2) var<storage, read> src: array<f32>;
@group(0) @binding(3) var<storage, read_write> members: array<u32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let g = gid.x;
    if (g >= p.nr * p.nr) {
        return;
    }
    let t = p.t;
    let rx = axis(g % p.nr);
    let ry = axis(g / p.nr);
    var dist: array<f32, 17>;
    var pos: array<u32, 17>;
    var len = 1u;
    dist[0] = 0.0;
    pos[0] = ry * t + rx;
    let r = i32(p.search);
    let nb = i32(p.nb);
    for (var oy = -r; oy <= r; oy = oy + 1) {
        let by = i32(ry) + oy;
        if (by < 0 || by >= nb) {
            continue;
        }
        for (var ox = -r; ox <= r; ox = ox + 1) {
            if (ox == 0 && oy == 0) {
                continue;
            }
            let bx = i32(rx) + ox;
            if (bx < 0 || bx >= nb) {
                continue;
            }
            // the sum of squared differences, along each row and then down the rows, as the reference adds them
            var s = 0.0;
            for (var j = 0u; j < N; j = j + 1u) {
                var h = 0.0;
                let a0 = (ry + j) * t + rx;
                let b0 = (u32(by) + j) * t + u32(bx);
                for (var i = 0u; i < N; i = i + 1u) {
                    let d = src[a0 + i] - src[b0 + i];
                    h = h + d * d;
                }
                s = s + h;
            }
            s = s * 0.0625;
            if (!(s <= p.tau)) {
                continue;
            }
            if (len >= p.cap && s >= dist[len - 1u]) {
                continue;
            }
            // after every block at most as far away, as the reference's ordered insert does
            var at = 0u;
            while (at < len && dist[at] <= s) {
                at = at + 1u;
            }
            var k = len;
            while (k > at) {
                dist[k] = dist[k - 1u];
                pos[k] = pos[k - 1u];
                k = k - 1u;
            }
            dist[at] = s;
            pos[at] = u32(by) * t + u32(bx);
            len = min(len + 1u, p.cap);
        }
    }
    var m = 1u;
    while (m * 2u <= len) {
        m = m * 2u;
    }
    members[g * STRIDE] = m;
    for (var i = 0u; i < m; i = i + 1u) {
        members[g * STRIDE + 1u + i] = pos[i];
    }
}
