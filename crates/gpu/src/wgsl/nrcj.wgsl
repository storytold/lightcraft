// Joint colour noise reduction (`lightcraft_pipeline::nrc_joint`). Each kernel mirrors its CPU
// twin there, in the same order of operations; keep them in step.
//
// Buffers: `a`, `b` inputs, `dst` output. Images are interleaved: RGB / Y,R−Y,B−Y (3 per pixel),
// chroma pairs R−Y,B−Y (2 per pixel), path pairs (h, v; exp(−path), see `nrc_joint`) or weight pairs (h, v).
// Kernels with the noise model: P[0] = n, P[1] = w, P[2] = h, P[3..6] = alpha, P[6..9] = beta,
// P[9..12] = ymax, P[12] = tolerance.

fn ja(i: u32) -> vec3<f32> {
    return vec3<f32>(a[3u * i], a[3u * i + 1u], a[3u * i + 2u]);
}

fn jb3(i: u32) -> vec3<f32> {
    return vec3<f32>(b[3u * i], b[3u * i + 1u], b[3u * i + 2u]);
}

fn ja2(i: u32) -> vec2<f32> {
    return vec2<f32>(a[2u * i], a[2u * i + 1u]);
}

fn jvc(v: vec3<f32>, i: u32) -> f32 {
    return select(select(v.x, v.y, i == 1u), v.z, i == 2u);
}

// `JointNoise::var`.
fn nvar(i: u32, y: f32) -> f32 {
    let yy = min(y, pf(9u + i));
    return max(pf(3u + i) * max(yy, 0.0) + pf(6u + i), 1e-14);
}

// `nrc_joint::distance`.
fn jdist(p: vec3<f32>, q: vec3<f32>) -> f32 {
    let y = 0.5 * (p.x + q.x);
    let t = 0.025 * max(y, 0.0);
    let allowance = t * t;
    var sum = 0.0;
    for (var i = 0u; i < 3u; i++) {
        let v = nvar(i, y);
        let d = jvc(p, i) - jvc(q, i);
        let excess = max(d * d - v, 0.0);
        if (excess != 0.0) {
            sum += excess / (3.0 * pf(12u) * v + allowance);
        }
    }
    return sum;
}

// `nrc_joint::weight`.
fn jweight(d: f32, p: f32) -> f32 {
    let m = max(d, p);
    return select(exp(-m), 1.0, m == 0.0);
}

// `nrc_joint::yuv` of pixel `i` of image `a`.
fn jyuv(i: u32) -> vec3<f32> {
    let c = ja(i);
    let y = lum2020(c);
    return vec3<f32>(y, c.x - y, c.z - y);
}

// `nrc_joint::yuv` of `n` pixels of image `a` from pixel P[1] on (a band's rows): P[0] = n.
@compute @workgroup_size(256)
fn nrcj_yuv(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let c = jyuv(pu(1u) + i);
    dst[3u * i] = c.x;
    dst[3u * i + 1u] = c.y;
    dst[3u * i + 2u] = c.z;
}

// The chroma pair (R−Y, B−Y) of the transformed image `a` (P[0] = n).
@compute @workgroup_size(256)
fn nrcj_uv(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[2u * i] = a[3u * i + 1u];
    dst[2u * i + 1u] = a[3u * i + 2u];
}

// Noise samples of image `a`, transformed on the fly (`nrc_joint::noise_model`): P[0] = sample count,
// P[1] = w, P[2] = h, P[3] = stride. Per sample 11 values: valid, brightness, 3 × 3 residuals.
@compute @workgroup_size(256)
fn nrcj_samples(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let k = lin_index(g, nw);
    if (k >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let index = k * pu(3u);
    let x = index % w;
    let y = index / w;
    let o = 11u * k;
    if (x == 0u || y == 0u || x + 1u == w || y + 1u == h) {
        dst[o] = 0.0;
        return;
    }
    let c = jyuv(index);
    var brightness = 0.0;
    for (var s = 0u; s < 3u; s++) {
        let d = min(min(min(min(1u << s, x), y), w - x - 1u), h - y - 1u);
        var m = vec3<f32>(0.0);
        m = m + 0.25 * jyuv(index - d);
        m = m + 0.25 * jyuv(index + d);
        m = m + 0.25 * jyuv(index - d * w);
        m = m + 0.25 * jyuv(index + d * w);
        let r = c - m;
        dst[o + 2u + 3u * s] = r.x * r.x;
        dst[o + 3u + 3u * s] = r.y * r.y;
        dst[o + 4u + 3u * s] = r.z * r.z;
        brightness += m.x / 3.0;
    }
    dst[o] = 1.0;
    dst[o + 1u] = max(brightness, 0.0);
}

// `nrc_joint::median8`: the same 19-comparator network, on plain variables (a private array indexed in a
// loop ends up in slow local memory on some GPUs).
fn med8(v0: f32, v1: f32, v2: f32, v3: f32, v4: f32, v5: f32, v6: f32, v7: f32) -> f32 {
    var a = v0;
    var b = v1;
    var c = v2;
    var d = v3;
    var e = v4;
    var f = v5;
    var g = v6;
    var h = v7;
    let t0 = min(a, c);
    c = max(a, c);
    a = t0;
    let t1 = min(b, d);
    d = max(b, d);
    b = t1;
    let t2 = min(e, g);
    g = max(e, g);
    e = t2;
    let t3 = min(f, h);
    h = max(f, h);
    f = t3;
    let t4 = min(a, e);
    e = max(a, e);
    a = t4;
    let t5 = min(b, f);
    f = max(b, f);
    b = t5;
    let t6 = min(c, g);
    g = max(c, g);
    c = t6;
    let t7 = min(d, h);
    h = max(d, h);
    d = t7;
    let t8 = min(a, b);
    b = max(a, b);
    a = t8;
    let t9 = min(c, d);
    d = max(c, d);
    c = t9;
    let t10 = min(e, f);
    f = max(e, f);
    e = t10;
    let t11 = min(g, h);
    h = max(g, h);
    g = t11;
    let t12 = min(c, e);
    e = max(c, e);
    c = t12;
    let t13 = min(d, f);
    f = max(d, f);
    d = t13;
    let t14 = min(b, e);
    e = max(b, e);
    b = t14;
    let t15 = min(d, g);
    g = max(d, g);
    d = t15;
    let t16 = min(b, c);
    c = max(b, c);
    b = t16;
    let t17 = min(d, e);
    e = max(d, e);
    d = t17;
    let t18 = min(f, g);
    g = max(f, g);
    f = t18;
    return 0.5 * (d + e);
}

// The farther of a value's distances to the least and largest of 8 (`speck_share`'s pre-test).
fn reach8(p: f32, v0: f32, v1: f32, v2: f32, v3: f32, v4: f32, v5: f32, v6: f32, v7: f32) -> f32 {
    let lo = min(min(min(v0, v1), min(v2, v3)), min(min(v4, v5), min(v6, v7)));
    let hi = max(max(max(v0, v1), max(v2, v3)), max(max(v4, v5), max(v6, v7)));
    return max(abs(p - lo), abs(p - hi));
}

// The two largest values so far (`t.x` ≥ `t.y`) with `s` added, as the CPU's fold.
fn top2(t: vec2<f32>, s: f32) -> vec2<f32> {
    return select(vec2<f32>(t.x, max(t.y, s)), vec2<f32>(s, t.x), s > t.x);
}

// One border pixel's share of the colour shift (`speck_share`).
fn sshare(q: vec3<f32>, m: vec3<f32>, du: f32, dv: f32, vu: f32, vv: f32, excess: f32) -> f32 {
    return ((q.y - m.y) * du / vu + (q.z - m.z) * dv / vv) / excess;
}

// `nrc_joint::despeck` + `speck_share`: `a` = transformed image, writes it with the chroma of compact
// colour specks replaced. Noise parameters as `nrcj_pilot`; P[13] = threshold, P[14] = share.
@compute @workgroup_size(256)
fn nrcj_speck(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let x = i % w;
    let y = i / w;
    let p = ja(i);
    var o = p;
    if (x >= 2u && y >= 2u && x + 2u < w && y + 2u < h) {
        let threshold = pf(13u);
        let share = pf(14u);
        let up = i - 2u * w;
        let dn = i + 2u * w;
        // ring: corners and edge centres of the 5 × 5 square (in the CPU's order)
        let r0 = ja(up - 2u);
        let r1 = ja(up);
        let r2 = ja(up + 2u);
        let r3 = ja(i - 2u);
        let r4 = ja(i + 2u);
        let r5 = ja(dn - 2u);
        let r6 = ja(dn);
        let r7 = ja(dn + 2u);
        let ym = (((((((r0.x + r1.x) + r2.x) + r3.x) + r4.x) + r5.x) + r6.x) + r7.x) / 8.0;
        let vu = nvar(1u, ym);
        let vv = nvar(2u, ym);
        let bu = reach8(p.y, r0.y, r1.y, r2.y, r3.y, r4.y, r5.y, r6.y, r7.y);
        let bv = reach8(p.z, r0.z, r1.z, r2.z, r3.z, r4.z, r5.z, r6.z, r7.z);
        if (bu * bu / vu + bv * bv / vv > threshold) {
            var m = vec3<f32>(0.0, med8(r0.y, r1.y, r2.y, r3.y, r4.y, r5.y, r6.y, r7.y), med8(r0.z, r1.z, r2.z, r3.z, r4.z, r5.z, r6.z, r7.z));
            let du = p.y - m.y;
            let dv = p.z - m.z;
            let excess = du * du / vu + dv * dv / vv;
            if (excess > threshold) {
                m.x = med8(r0.x, r1.x, r2.x, r3.x, r4.x, r5.x, r6.x, r7.x);
                let dy = p.x - m.x;
                let luminance = dy * dy / nvar(0u, ym);
                let alone = clamp((1.5 * excess - luminance) / excess, 0.0, 1.0);
                if (alone > 0.0) {
                    // the two largest border shares (`speck_share`)
                    var top = vec2<f32>(0.0, 0.0);
                    top = top2(top, sshare(r0, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r1, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r2, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r3, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r4, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r5, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r6, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(r7, m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(up - 1u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(up + 1u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(i - w - 2u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(i - w + 2u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(i + w - 2u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(i + w + 2u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(dn - 1u), m, du, dv, vu, vv, excess));
                    top = top2(top, sshare(ja(dn + 1u), m, du, dv, vu, vv, excess));
                    let compact = clamp((0.8 - top.y) / 0.3, 0.0, 1.0);
                    let t = clamp((excess - threshold) / threshold, 0.0, 1.0);
                    let k = share * t * t * (3.0 - 2.0 * t) * alone * compact;
                    if (k > 0.0) {
                        o.y += k * (m.y - p.y);
                        o.z += k * (m.z - p.z);
                    }
                }
            }
        }
    }
    dst[3u * i] = o.x;
    dst[3u * i + 1u] = o.y;
    dst[3u * i + 2u] = o.z;
}

// `nrc_joint::pilot`: `a` = transformed image, `b` = its gaussian(0.8); writes the guide.
@compute @workgroup_size(256)
fn nrcj_pilot(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let x = i % w;
    let y = i / w;
    var out = jb3(i);
    if (!(x == 0u || y == 0u || x + 1u == w || y + 1u == h)) {
        let p = ja(i);
        // 9 · noise variance per component, at the pixel's luminance
        let nv = 9.0 * vec3<f32>(nvar(0u, p.x), nvar(1u, p.x), nvar(2u, p.x));
        // axis 0 (left/right) and axis 1 (up/down), written out: private arrays indexed in a loop end up in
        // slow local memory on some GPUs
        let qa = ja(i - 1u);
        let qb = ja(i + 1u);
        let mh = (qa + p + qb) / 3.0;
        let ha = qa - mh;
        let hp = p - mh;
        let hb = qb - mh;
        let th = (ha * ha + hp * hp + hb * hb) / nv;
        let vh = th.x + th.y + th.z;
        let ua = ja(i - w);
        let ub = ja(i + w);
        let mv = (ua + p + ub) / 3.0;
        let va = ua - mv;
        let vp = p - mv;
        let vb = ub - mv;
        let tv = (va * va + vp * vp + vb * vb) / nv;
        let vvar = tv.x + tv.y + tv.z;
        // the axis choice fades in between 0.8 and 1.25 times its bound (nrc_joint::pilot_pick)
        let sv = smoothstep(0.8, 1.25, vh / max(4.0, 4.0 * vvar));
        let sh = smoothstep(0.8, 1.25, vvar / max(4.0, 4.0 * vh));
        if (sv >= 1.0) {
            out = mv;
        } else if (sh >= 1.0) {
            out = mh;
        } else {
            out = out + (mv - out) * sv + (mh - out) * sh;
        }
    }
    dst[3u * i] = out.x;
    dst[3u * i + 1u] = out.y;
    dst[3u * i + 2u] = out.z;
}

// Paths of the direct neighbours, exp(−distance) (1 where there is no pair): `a` = guide; writes path pairs.
@compute @workgroup_size(256)
fn nrcj_links1(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let x = i % w;
    let y = i / w;
    let p = ja(i);
    var q = vec2<f32>(1.0);
    if (x + 1u < w) {
        let d = jdist(p, ja(i + 1u));
        q.x = jweight(d, d);
    }
    if (y + 1u < h) {
        let d = jdist(p, ja(i + w));
        q.y = jweight(d, d);
    }
    dst[2u * i] = q.x;
    dst[2u * i + 1u] = q.y;
}

// Paths of length 2·delta from those of length delta (`a`, path pairs): their minimum. P[3] = delta.
@compute @workgroup_size(256)
fn nrcj_paths(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let delta = pu(3u);
    let x = i % w;
    let y = i / w;
    var ph = a[2u * i];
    var pv = a[2u * i + 1u];
    if (x + delta < w) {
        ph = min(ph, a[2u * (i + delta)]);
    }
    if (y + delta < h) {
        pv = min(pv, a[2u * (i + delta * w) + 1u]);
    }
    dst[2u * i] = ph;
    dst[2u * i + 1u] = pv;
}

// `nrc_joint::path_weight`.
fn jpweight(d: f32, path: f32) -> f32 {
    return select(min(exp(-d), path), path, d == 0.0);
}

// The pair weights at distance `next` from the guide (`a`) and the paths (`b`); writes weight pairs.
// Noise parameters as `nrcj_pilot`; P[13] = next.
@compute @workgroup_size(256)
fn nrcj_links(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let next = pu(13u);
    let x = i % w;
    let y = i / w;
    let p = ja(i);
    var lh = 0.0;
    var lv = 0.0;
    let ph = b[2u * i];
    let pv = b[2u * i + 1u];
    if (x + next < w && ph > 0.0) {
        lh = jpweight(jdist(p, ja(i + next)), ph);
    }
    if (y + next < h && pv > 0.0) {
        lv = jpweight(jdist(p, ja(i + next * w)), pv);
    }
    dst[2u * i] = lh;
    dst[2u * i + 1u] = lv;
}

// One horizontal diffusion step (`nrc_joint::diffuse_h`): `a` = chroma pairs, `b` = weight pairs.
// P[0] = n, P[1] = w, P[2] = h, P[3] = delta, P[4] = rate.
@compute @workgroup_size(256)
fn nrcj_dh(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let delta = pu(3u);
    let rate = pf(4u);
    let x = i % w;
    let p = ja2(i);
    var o = p;
    if (x >= delta) {
        o += rate * b[2u * (i - delta)] * (ja2(i - delta) - p);
    }
    if (x + delta < w) {
        o += rate * b[2u * i] * (ja2(i + delta) - p);
    }
    dst[2u * i] = o.x;
    dst[2u * i + 1u] = o.y;
}

// One vertical diffusion step (`nrc_joint::v_row`), parameters as `nrcj_dh`.
@compute @workgroup_size(256)
fn nrcj_dv(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let h = pu(2u);
    let delta = pu(3u);
    let rate = pf(4u);
    let y = i / w;
    let p = ja2(i);
    var o = p;
    if (y >= delta) {
        o += rate * b[2u * (i - delta * w) + 1u] * (ja2(i - delta * w) - p);
    }
    if (y + delta < h) {
        o += rate * b[2u * i + 1u] * (ja2(i + delta * w) - p);
    }
    dst[2u * i] = o.x;
    dst[2u * i + 1u] = o.y;
}

// Back to RGB with the image's own luminance, blended by the strength: `a` = image, `b` = a band's
// filtered chroma pairs, `dst` = the whole output. P[0] = pixels written, P[1] = strength,
// P[2] = first pixel written (in the image), P[3] = first pixel of the band (in the image).
@compute @workgroup_size(256)
fn nrcj_recon(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let k = lin_index(g, nw);
    if (k >= pu(0u)) {
        return;
    }
    let s = pf(1u);
    let i = pu(2u) + k;
    let bi = i - pu(3u);
    let p = ja(i);
    let y = lum2020(p);
    let r = y + b[2u * bi];
    let bl = y + b[2u * bi + 1u];
    let res = vec3<f32>(r, (y - 0.2627 * r - 0.0593 * bl) / 0.6780, bl);
    let o = p + s * (res - p);
    dst[3u * i] = o.x;
    dst[3u * i + 1u] = o.y;
    dst[3u * i + 2u] = o.z;
}
