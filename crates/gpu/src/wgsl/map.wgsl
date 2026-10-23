// Element-wise kernels (one thread per pixel, 1-D on a 2-D grid). Bindings: a, b, c (inputs), dst.
// P[0] = pixel count; further parameters per kernel. Each mirrors a CPU expression in
// `lightcraft_pipeline::local` (named in the comment).

fn rgb_a(i: u32) -> vec3<f32> {
    return vec3<f32>(a[3u * i], a[3u * i + 1u], a[3u * i + 2u]);
}

fn put_rgb(i: u32, v: vec3<f32>) {
    dst[3u * i] = v.x;
    dst[3u * i + 1u] = v.y;
    dst[3u * i + 2u] = v.z;
}

// `log_lum` of an RGB image.
@compute @workgroup_size(256)
fn log_lum_k(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = log_lum(rgb_a(i));
}

// `dark_of`: the dark channel (dehaze).
@compute @workgroup_size(256)
fn dark_k(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let c = rgb_a(i);
    dst[i] = min(min(c.x, c.y), c.z);
}

// Guided filter input pair (p, p²).
@compute @workgroup_size(256)
fn guided_pre(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let p = a[i];
    dst[2u * i] = p;
    dst[2u * i + 1u] = p * p;
}

// Guided filter coefficients (a, b) from blurred (mean, corr). P[1] = eps.
@compute @workgroup_size(256)
fn guided_ab(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let m = a[2u * i];
    let c = a[2u * i + 1u];
    let v = max(c - m * m, 0.0);
    let k = v / (v + pf(1u));
    dst[2u * i] = k;
    dst[2u * i + 1u] = m - k * m;
}

// `q = a·p + b` (a: p, b: interleaved blurred coefficients).
@compute @workgroup_size(256)
fn guided_apply(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = b[2u * i] * a[i] + b[2u * i + 1u];
}

// Cross guided filter (`masks::guided_cross`), a: guide, b: input. P[1] = 0: (guide, input);
// 1: (guide·input, guide²).
@compute @workgroup_size(256)
fn xguided_pre(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let gi = a[i];
    let p = b[i];
    if (pu(1u) == 0u) {
        dst[2u * i] = gi;
        dst[2u * i + 1u] = p;
    } else {
        dst[2u * i] = gi * p;
        dst[2u * i + 1u] = gi * gi;
    }
}

// Coefficients (a, b) from blurred means (a: guide, input) and correlations (b: guide·input,
// guide²). P[1] = eps.
@compute @workgroup_size(256)
fn xguided_ab(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let mi = a[2u * i];
    let mp = a[2u * i + 1u];
    let v = max(b[2u * i + 1u] - mi * mi, 0.0);
    let k = (b[2u * i] - mi * mp) / (v + pf(1u));
    dst[2u * i] = k;
    dst[2u * i + 1u] = mp - k * mi;
}

// `max(p, clamp(a·guide + b, 0, 1))` (a: guide, b: blurred coefficients, c: the input p).
@compute @workgroup_size(256)
fn xguided_apply(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = max(c[i], clamp(b[2u * i] * a[i] + b[2u * i + 1u], 0.0, 1.0));
}

// White balance: 3×3 matrix (P[1] = has matrix, P[2..11] row-major), clamped at 0.
@compute @workgroup_size(256)
fn wb_k(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    var c = rgb_a(i);
    if (pu(1u) != 0u) {
        c = vec3<f32>(
            pf(2u) * c.x + pf(3u) * c.y + pf(4u) * c.z,
            pf(5u) * c.x + pf(6u) * c.y + pf(7u) * c.z,
            pf(8u) * c.x + pf(9u) * c.y + pf(10u) * c.z,
        );
    }
    put_rgb(i, max(c * 1.0, vec3<f32>(0.0)));
}

// Negative Craft (`lightcraft_pipeline::film::FilmKernel::apply`). P[1..] is `FilmKernel::words`:
// base, gamma, presat, slope, power (film gamma on a negative, grade on a slide), offset, shoulder,
// balance, shadow, mid, high, flags, capture, fog.g, white.g, fog.rgb, white.rgb, dye.rgb, aim.rgb,
// chroma, axis.rgb.
@compute @workgroup_size(256)
fn film_k(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    var t = rgb_a(i);
    // `FilmKernel::apply`: non-finite or negative transmission is 0 (NaN fails `t >= 0`).
    t = select(vec3<f32>(0.0), t, t >= vec3<f32>(0.0) & t < vec3<f32>(3.0e38));
    let capture = pf(25u);
    let presat = pf(7u);
    if (capture > 1.001) {
        t = pow(t, vec3<f32>(capture));
    }
    if (abs(presat - 1.0) > 1e-6) {
        let y = (t.x + t.y + t.z) / 3.0;
        t = max(vec3<f32>(y) + (t - vec3<f32>(y)) * presat, vec3<f32>(0.0));
    }
    var base = vec3<f32>(pf(1u), pf(2u), pf(3u));
    var gamma = vec3<f32>(pf(4u), pf(5u), pf(6u));
    let flags = pu(24u);
    if ((flags & 2u) != 0u) {
        let y = lum2020(t);
        t = vec3<f32>(y);
        let b = max(lum2020(base), 1e-4);
        base = vec3<f32>(b);
        gamma = vec3<f32>(max(gamma.y, 0.05));
    } else {
        base = max(base, vec3<f32>(1e-4));
        gamma = max(gamma, vec3<f32>(0.05));
    }
    let n = max(t / base, vec3<f32>(1e-6));
    var scene: vec3<f32>;
    if ((flags & 1u) != 0u) {
        // `printing_density` and the gray axis in `lightcraft_pipeline::film`.
        var d = min(max(-log(min(n, vec3<f32>(1e6))) / log(10.0), vec3<f32>(0.0)) * gamma, vec3<f32>(4.0));
        let cg = pf(34u);
        let mb = pf(35u);
        let cb = pf(36u);
        if (cg != 0.0 || mb != 0.0 || cb != 0.0) {
            // Hanson: C = Dr, M = Dg − cg·C, Y = Db − cb·C − mb·M.
            let c = d.x;
            let m = max(d.y - cg * c, 0.0);
            let y = max(d.z - cb * c - mb * m, 0.0);
            d = vec3<f32>(c, m, y);
        }
        let fog = vec3<f32>(pf(28u), pf(29u), pf(30u));
        let white = vec3<f32>(pf(31u), pf(32u), pf(33u));
        let axis = max(vec3<f32>(pf(41u), pf(42u), pf(43u)), vec3<f32>(0.05));
        let straight = max(d - fog, vec3<f32>(0.0)) / axis;
        // pf(9) is the film gamma on a negative (FILM_GAMMA / grade). The brightest channel
        // is the curve white. A near-neutral bright end is gained so each channel's white
        // density prints at 1, fading in toward that end. Equal whites leave the gain at 1.
        // Mono skips it. Density above the white stays above 1.
        let gam = max(pf(9u), 0.05);
        let curve = max(max(max(white.x, white.y), white.z), 0.05);
        let scale = max(pow(10.0, min(curve / gam, 8.0)) - 1.0, 1e-4);
        let e = min(straight / gam, vec3<f32>(8.0));
        scene = min((pow(vec3<f32>(10.0), e) - vec3<f32>(1.0)) / scale, vec3<f32>(64.0));
        if ((flags & 2u) == 0u) {
            let straight_y = 0.2627 * straight.x + 0.6780 * straight.y + 0.0593 * straight.z;
            let t = sstep(curve * 0.62, curve * 0.92, straight_y);
            let channel = max(clamp(white, vec3<f32>(curve - 0.45), vec3<f32>(curve)), vec3<f32>(0.05));
            let at_e = min(channel / gam, vec3<f32>(8.0));
            let at = max((pow(vec3<f32>(10.0), at_e) - vec3<f32>(1.0)) / scale, vec3<f32>(1e-4));
            scene = min(scene * (vec3<f32>(1.0) + (vec3<f32>(1.0) / at - vec3<f32>(1.0)) * t), vec3<f32>(64.0));
        }
        scene = scene * vec3<f32>(pf(37u), pf(38u), pf(39u));
        let chroma = pf(40u);
        if (abs(chroma - 1.0) > 1e-4) {
            let y = (scene.x + scene.y + scene.z) / 3.0;
            scene = max(vec3<f32>(y) + (scene - vec3<f32>(y)) * chroma, vec3<f32>(0.0));
        }
    } else {
        scene = pow(n, gamma);
    }
    let slope = pf(8u);
    let power = pf(9u);
    let offset = pf(10u);
    let shoulder = pf(11u);
    var v = max(scene * slope + vec3<f32>(offset), vec3<f32>(0.0));
    // Slide contrast. A negative's contrast is the film gamma above.
    if ((flags & 1u) == 0u && abs(power - 1.0) > 1e-4) {
        v = pow(max(v, vec3<f32>(0.0)), vec3<f32>(power));
    }
    let y = lum2020(v);
    let sh = 1.0 - sstep(0.02, 0.35, y);
    let hi = sstep(0.55, 0.95, y);
    let mid = max(1.0 - sh - hi, 0.0);
    let balance = vec3<f32>(pf(12u), pf(13u), pf(14u));
    let shadow = vec3<f32>(pf(15u), pf(16u), pf(17u));
    let midg = vec3<f32>(pf(18u), pf(19u), pf(20u));
    let high = vec3<f32>(pf(21u), pf(22u), pf(23u));
    let band = shadow * sh + midg * mid + high * hi;
    v = v * max(balance * band, vec3<f32>(0.0));
    if (shoulder > 1e-4) {
        let e = max(v - vec3<f32>(shoulder), vec3<f32>(0.0));
        let rolled = vec3<f32>(shoulder) + e / (vec3<f32>(1.0) + e / vec3<f32>(shoulder));
        v = select(v, rolled, v > vec3<f32>(shoulder));
    }
    put_rgb(i, v);
}

// Luminance NR: scale by 2^((f − l)·k) (a: image, b: log luminance l, c: filtered f). P[1] = k.
@compute @workgroup_size(256)
fn nr_lum(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let d = (c[i] - b[i]) * pf(1u);
    put_rgb(i, rgb_a(i) * exp2(d));
}

// Chromaticity rgb / Y.
@compute @workgroup_size(256)
fn chroma_k(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let c0 = rgb_a(i);
    let y = max(lum2020(c0), 1e-6);
    put_rgb(i, vec3<f32>(c0.x / y, c0.y / y, c0.z / y));
}

// Colour NR: blend chromaticity towards its blur and re-apply luminance (a: image, b: chroma,
// c: blurred chroma). P[1] = t.
@compute @workgroup_size(256)
fn nr_col(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let yl = lum2020(rgb_a(i));
    let t = pf(1u);
    let c0 = vec3<f32>(b[3u * i], b[3u * i + 1u], b[3u * i + 2u]);
    let cb = vec3<f32>(c[3u * i], c[3u * i + 1u], c[3u * i + 2u]);
    put_rgb(i, max((c0 + (cb - c0) * t) * yl, vec3<f32>(0.0)));
}

// Every P[1]-th value of `a` (airlight sampling). P[0] = output count.
@compute @workgroup_size(256)
fn subsample(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    dst[i] = a[i * pu(1u)];
}

// `redeye::eye_pixel` (a: image). P[1] = width, P[2] = eye count, then EYE_WORDS per eye:
// cx, cy, r, darken, pet, has catchlight, catchlight x, y, r (output pixels).
@compute @workgroup_size(256)
fn redeye_k(@builtin(global_invocation_id) g: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    let i = lin_index(g, nw);
    if (i >= pu(0u)) {
        return;
    }
    let w = pu(1u);
    let x = f32(i % w) + 0.5;
    let y = f32(i / w) + 0.5;
    var c = rgb_a(i);
    for (var e = 0u; e < pu(2u); e++) {
        let o = 3u + e * EYE_WORDS;
        let ex = x - pf(o);
        let ey = y - pf(o + 1u);
        let d = sqrt(ex * ex + ey * ey) / pf(o + 2u);
        var m = 1.0 - sstep(1.0, 1.35, d);
        if (pf(o + 4u) != 0.0) {
            m = 1.0 - sstep(0.95, 1.2, d);
        }
        let darken = pf(o + 3u);
        if (m > 0.0) {
            if (pf(o + 4u) != 0.0) {
                let k = min(lum2020(c), 0.04) * (1.0 - 0.85 * darken);
                c = c + (vec3<f32>(k) - c) * m;
            } else {
                let red = (c.x - max(c.y, c.z)) / max(c.x, 1e-6);
                let a = m * sstep(0.25, 0.5, red);
                let n = 0.5 * (c.y + c.z) * (1.0 - 0.9 * darken);
                c = c + (vec3<f32>(n) - c) * a;
            }
        }
        if (pf(o + 5u) != 0.0) {
            let lx = x - pf(o + 6u);
            let ly = y - pf(o + 7u);
            let dc = sqrt(lx * lx + ly * ly) / pf(o + 8u);
            let mc = 1.0 - sstep(0.5, 1.0, dc);
            if (mc > 0.0) {
                c = c + (vec3<f32>(0.9) - c) * mc;
            }
        }
    }
    put_rgb(i, c);
}
