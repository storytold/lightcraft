// The per-pixel stage: a straight port of `lightcraft_pipeline::finish` (keep in step with it).
// Bindings: img (rgb, pre-exposure), log_l, base, clar, tex, dark, masks (NMASK planes, then the
// blurred chromaticity when HAS_CHROMA), aux
// (tone LUT | chroma curve | sRGB LUT | curve LUTs | mask terms), out (packed RGBA8).

fn tone_apply(y: f32) -> f32 {
    if (y <= 0.0) {
        return 0.0;
    }
    let ev = log2(y / GREY);
    let f = clamp((ev - TONE_MIN_EV) / (TONE_MAX_EV - TONE_MIN_EV), 0.0, 1.0) * f32(TONE_N - 1u);
    let i = min(u32(f), TONE_N - 2u);
    let t = f - f32(i);
    let v = aux[i] + (aux[i + 1u] - aux[i]) * t;
    if (ev < TONE_MIN_EV) {
        return v * (y / (GREY * TONE_MIN_GAIN));
    }
    return v;
}

// The camera chroma curve follows the tone LUT in `aux` (`ToneMap::chroma_scale`).
fn chroma_scale(o: f32) -> f32 {
    let f = clamp(o, 0.0, 1.0) * f32(CHROMA_N - 1u);
    let i = min(u32(f), CHROMA_N - 2u);
    let t = f - f32(i);
    let a = aux[TONE_N + i];
    let b = aux[TONE_N + i + 1u];
    if (a == b) {
        return a;
    }
    return a + (b - a) * t;
}

fn encode_srgb(v: f32) -> f32 {
    let o = pu(F_SRGB_OFF);
    let f = clamp(v, 0.0, 1.0) * f32(SRGB_N);
    let i = min(u32(f), SRGB_N - 1u);
    let t = f - f32(i);
    return aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
}

fn curve(ch: u32, x: f32) -> f32 {
    let o = pu(F_CURVE_OFF) + ch * CURVE_N;
    let f = clamp(x, 0.0, 1.0) * f32(CURVE_N - 1u);
    let i = min(u32(f), CURVE_N - 2u);
    let t = f - f32(i);
    return aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
}

fn band_weights(h: f32) -> array<f32, 8> {
    var w: array<f32, 8>;
    for (var i = 0u; i < 8u; i++) {
        let a = pf(F_BANDH + i);
        let b = pf(F_BANDH + (i + 1u) % 8u);
        let span = rem_euclid(wrap_angle(b - a), TAU);
        let d = rem_euclid(wrap_angle(h - a), TAU);
        if (d <= span) {
            let t = d / span;
            let s = 0.5 - 0.5 * cos(t * PI);
            w[i] += 1.0 - s;
            w[(i + 1u) % 8u] += s;
            break;
        }
    }
    return w;
}

// `PointK::apply` for Point Color sample `k` (OkLCh in, OkLCh out).
fn point_color(k: u32, lch: vec3<f32>) -> vec3<f32> {
    let o = F_PC + k * POINT_WORDS;
    let sl = pf(o);
    let sc = pf(o + 1u);
    let sh = pf(o + 2u);
    let var_k = pf(o + 6u);
    let wh_ = pf(o + 7u);
    let wc_ = pf(o + 8u);
    let wl_ = pf(o + 9u);
    var l = lch.x;
    var c = lch.y;
    var h = lch.z;
    var wh = 1.0;
    if (sc >= 0.02) {
        wh = (1.0 - sstep(0.5 * wh_, wh_, abs(wrap_angle(h - sh)))) * sstep(0.005, 0.025, c);
    }
    if (wh <= 0.0) {
        return lch;
    }
    let wc = 1.0 - sstep(0.5 * wc_, wc_, abs(c - sc));
    let wl = 1.0 - sstep(0.5 * wl_, wl_, abs(l - sl));
    let w = wh * wc * wl;
    if (w <= 0.0) {
        return lch;
    }
    h += w * (var_k * wrap_angle(h - sh) + pf(o + 3u));
    c += w * var_k * (c - sc);
    c = max(c * (1.0 + w * pf(o + 4u)), 0.0);
    l += w * (var_k * (l - sl) + pf(o + 5u));
    return vec3<f32>(l, c, h);
}

// `ColorOps::apply`.
fn color_ops(rgb: vec3<f32>, local_sat: f32, local_hue: f32) -> vec3<f32> {
    if (pu(F_OPS_IDENTITY) != 0u && local_sat == 0.0 && local_hue == 0.0) {
        return rgb;
    }
    let lab0 = oklab(rgb);
    var l = lab0.x;
    var c = sqrt(lab0.y * lab0.y + lab0.z * lab0.z);
    var h = atan2(lab0.z, lab0.y);
    if (pu(F_MIXER) != 0u) {
        let w = band_weights(h);
        var dh = 0.0;
        var ds = 0.0;
        var dl = 0.0;
        for (var i = 0u; i < 8u; i++) {
            dh += w[i] * pf(F_MIX_HUE + i);
            ds += w[i] * pf(F_MIX_SAT + i);
            dl += w[i] * pf(F_MIX_LUM + i);
        }
        let chroma_w = min(c / 0.12, 1.0);
        h += dh * chroma_w;
        c *= max(1.0 + ds, 0.0);
        l += dl * chroma_w * sqrt(max(l, 0.05));
    }
    for (var k = 0u; k < pu(F_NPC); k++) {
        let r = point_color(k, vec3<f32>(l, c, h));
        l = r.x;
        c = r.y;
        h = r.z;
    }
    let vib = pf(F_VIBRANCE);
    if (vib != 0.0) {
        let low = 1.0 - clamp(c / 0.22, 0.0, 1.0);
        var skin = 1.0;
        if (vib > 0.0) {
            let q = wrap_angle(h - pf(F_SKIN)) / 0.35;
            skin = 1.0 - 0.6 * exp(-(q * q));
        }
        c *= max(1.0 + vib * low * low * skin * 1.2, 0.0);
    }
    let sat = pf(F_SATURATION);
    if (sat != 0.0 || local_sat != 0.0) {
        c *= max(1.0 + sat + local_sat, 0.0);
    }
    h += local_hue;
    if (pu(F_BW) != 0u) {
        let w = band_weights(h);
        var mix = 0.0;
        for (var i = 0u; i < 8u; i++) {
            mix += w[i] * pf(F_BW_MIX + i);
        }
        l = max(l + mix * min(c / 0.2, 1.0) * 0.25, 0.0);
        c = 0.0;
    }
    var lab = vec3<f32>(l, c * cos(h), c * sin(h));
    if (pu(F_GRADING) != 0u) {
        let m = 0.5 - pf(F_BALANCE) * 0.25;
        let width = 0.15 + pf(F_BLENDING) * 0.5;
        let ws = 1.0 - sstep(m - width, m + width * 0.25, lab.x);
        let wh = sstep(m - width * 0.25, m + width, lab.x);
        let wm = max(1.0 - ws - wh, 0.0);
        let wts = array<f32, 4>(ws, wm, wh, 1.0);
        for (var k = 0u; k < 4u; k++) {
            let wt = wts[k];
            lab.y += pf(F_WHEELS + 3u * k) * wt;
            lab.z += pf(F_WHEELS + 3u * k + 1u) * wt;
            lab.x += pf(F_WHEELS + 3u * k + 2u) * wt;
        }
    }
    return oklab_inv(lab);
}

// `colorops::calibrate`: primaries matrix, then the shadows tint (luminance kept).
fn calibrate(c0: vec3<f32>) -> vec3<f32> {
    var c = c0;
    if (pu(F_CALIB) != 0u) {
        let m = array<vec3<f32>, 3>(
            vec3<f32>(pf(F_CALIB_M), pf(F_CALIB_M + 1u), pf(F_CALIB_M + 2u)),
            vec3<f32>(pf(F_CALIB_M + 3u), pf(F_CALIB_M + 4u), pf(F_CALIB_M + 5u)),
            vec3<f32>(pf(F_CALIB_M + 6u), pf(F_CALIB_M + 7u), pf(F_CALIB_M + 8u)),
        );
        c = max(mul3(m, c), vec3<f32>(0.0));
    }
    let st = pf(F_SHADOW_TINT);
    if (st != 0.0) {
        let y0 = lum2020(c);
        let w = 1.0 - sstep(-5.0, -0.5, log2(max(y0, 1e-7) / 0.18));
        c.y *= max(1.0 - SHADOW_TINT_K * st * w, 0.0);
        let y1 = max(lum2020(c), 1e-9);
        c = c * y0 / y1;
    }
    return c;
}

// `finish::refine_saturation`.
fn refine_saturation(before: vec3<f32>, after: vec3<f32>, refine: f32) -> vec3<f32> {
    let lw = vec3<f32>(0.2126, 0.7152, 0.0722);
    let y0 = dot(before, lw);
    let y1 = dot(after, lw);
    let s0 = (max(before.x, max(before.y, before.z)) - min(before.x, min(before.y, before.z))) / max(y0, 1e-4);
    let s1 = (max(after.x, max(after.y, after.z)) - min(after.x, min(after.y, after.z))) / max(y1, 1e-4);
    if (s1 <= 1e-6 || s0 <= 1e-6) {
        return after;
    }
    let k = clamp(pow(s0 / s1, 1.0 - refine), 0.0, 4.0);
    return y1 + (after - y1) * k;
}

// --- film grain: a port of `lightcraft_pipeline::grain` (keep in step with it)
fn gpcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn gcell(i: i32, j: i32, salt: u32) -> u32 {
    return gpcg((bitcast<u32>(i) * GRAIN_H0) ^ (bitcast<u32>(j) * GRAIN_H1) ^ salt);
}

fn gsub_hash(h: u32, j: u32) -> u32 {
    return gpcg(h ^ ((j + 1u) * 0x9e3779b9u));
}

fn gunit(v: u32) -> f32 {
    return (f32(v >> 8u) + 0.5) * (1.0 / 16777216.0);
}

fn gunit16(v: u32) -> f32 {
    return (f32(v & 0xffffu) + 0.5) * (1.0 / 65536.0);
}

fn gunit8(v: u32) -> f32 {
    return (f32(v & 0xffu) + 0.5) * (1.0 / 256.0);
}

fn gunit4(v: u32) -> f32 {
    return (f32(v & 15u) + 0.5) * (1.0 / 16.0);
}

fn grain_per_cell(p: f32) -> f32 {
    let c = pf(F_GRAIN_CELL);
    return -log(1.0 - p) / pf(F_GRAIN_AREA) * c * c;
}

fn grain_std(p: f32) -> f32 {
    let t = grain_per_cell(p);
    let fw = min(t, 1.0);
    let mu = -log(1.0 - p) * (1.0 - fw / (6.0 * max(t, 1e-12)));
    return (1.0 - p) * sqrt(max(exp(pf(F_GRAIN_KAPPA) * mu) - 1.0, 0.0));
}

fn grain_sub_count(h: u32) -> u32 {
    let e = pf(F_GRAIN_CLUMP_E);
    if (e <= 0.0) {
        return 1u;
    }
    let extra = floor(gunit(gpcg(h ^ GRAIN_H3)) * (2.0 * e + 1.0));
    return 1u + min(u32(max(extra, 0.0)), GRAIN_MAX_SUB - 1u);
}

fn grain_sub_offset(hj: u32) -> vec2<f32> {
    let o = gpcg(hj ^ GRAIN_H0);
    let d = pf(F_GRAIN_CLUMP_D);
    return vec2<f32>(d * (gunit8(o) + gunit8(o >> 8u) - 1.0), d * (gunit8(o >> 16u) + gunit8(o >> 24u) - 1.0));
}

// coverage of grain `hj` at offset (dx, dy) from its centre: radius, density, elongation,
// orientation, lobed outline, soft edge (0 beyond its reach)
fn grain_sub_cover(hj: u32, dx: f32, dy: f32, d2: f32) -> f32 {
    let s = pf(F_GRAIN_S);
    let lump = pf(F_GRAIN_LUMP);
    let rad = gpcg(hj ^ GRAIN_H3);
    let z = 2.0 * (gunit8(rad) + gunit8(rad >> 8u) + gunit8(rad >> 16u)) - 3.0;
    let r0 = exp(pf(F_GRAIN_SPREAD) * z);
    let o = 0.25 + 0.75 * gunit8(rad >> 24u);
    let shp = gpcg(hj ^ GRAIN_H1);
    let a = 2.0 * gunit8(shp) - 1.0;
    let b = 2.0 * gunit8(shp >> 8u) - 1.0;
    let n = sqrt(a * a + b * b);
    var ux = 1.0;
    var uy = 0.0;
    if (n > 1e-3) {
        ux = a / n;
        uy = b / n;
    }
    let asp = 1.0 + pf(F_GRAIN_ASPECT) * gunit4(shp >> 16u);
    let rr = r0 * sqrt(asp) * (1.0 + lump);
    let reach = sqrt(rr * rr + s * s) + s;
    if (d2 >= reach * reach) {
        return 0.0;
    }
    let d = sqrt(d2);
    var r = r0;
    if (d > 1e-6) {
        let inv_d = 1.0 / d;
        let c = dx * inv_d;
        let sn = dy * inv_d;
        if (asp > 1.0) {
            let cu = c * ux + sn * uy;
            let cw = sn * ux - c * uy;
            r = r / sqrt(cu * cu * (1.0 / asp) + cw * cw * asp);
        }
        if (lump > 0.0) {
            let m = 0.5 + f32((shp >> 28u) & 15u) * (1.0 / 30.0);
            let p2 = GRAIN_PHASES[(shp >> 20u) & 15u];
            let p3 = GRAIN_PHASES[(shp >> 24u) & 15u];
            let c2 = c * c - sn * sn;
            let s2 = 2.0 * c * sn;
            let c3 = c * (4.0 * c * c - 3.0);
            let s3 = sn * (3.0 - 4.0 * sn * sn);
            r = r * (1.0 + lump * (0.6 * m * (p2.x * c2 + p2.y * s2) + 0.4 * m * (p3.x * c3 + p3.y * s3)));
        }
    }
    let rho2 = r * r + s * s;
    let tau = (d - sqrt(rho2)) * pf(F_GRAIN_INV_S);
    if (tau >= 1.0) {
        return 0.0;
    }
    var prof = 1.0;
    if (tau > -1.0) {
        prof = (1.0 - tau) * (1.0 - tau) * (2.0 + tau) * 0.25;
    }
    return o * r * r / (rho2 + s * s * 0.2) * prof;
}

// coverage of clump `h` at offset (dx, dy) from its centre: the union of its grains
fn grain_clump_cover(h: u32, dx: f32, dy: f32) -> f32 {
    let sr = pf(F_GRAIN_SUB_REACH);
    let count = grain_sub_count(h);
    var t = 1.0;
    for (var j = 0u; j < count; j++) {
        let hj = gsub_hash(h, j);
        let off = grain_sub_offset(hj);
        let ex = dx - off.x;
        let ey = dy - off.y;
        let d2 = ex * ex + ey * ey;
        if (d2 >= sr * sr) {
            continue;
        }
        t *= 1.0 - grain_sub_cover(hj, ex, ey, d2);
    }
    return 1.0 - t;
}

fn grain_field(x: f32, y: f32, p: f32, seed: u32) -> f32 {
    let t = grain_per_cell(p);
    let fw = max(min(t, 1.0), 1e-6);
    let end = t + 0.5 * fw;
    let r = pf(F_GRAIN_SUPPORT);
    let c = pf(F_GRAIN_CELL);
    // the cells within reach, as a start and a bounded count (never a loop up to i32::MAX)
    let fi0 = floor((x - r) / c);
    let fj0 = floor((y - r) / c);
    let i0 = i32(clamp(fi0, -2.0e9, 2.0e9));
    let j0 = i32(clamp(fj0, -2.0e9, 2.0e9));
    let ni = min(u32(max(floor((x + r) / c) - fi0, 0.0)), u32(GRAIN_MAX_REACH));
    let nj = min(u32(max(floor((y + r) / c) - fj0, 0.0)), u32(GRAIN_MAX_REACH));
    let e_end = exp(-end);
    let e_full = exp(fw - end);
    let salt = gpcg(seed);
    var keep = 1.0;
    for (var dj = 0u; dj <= nj; dj++) {
        let j = j0 + i32(dj);
        let dy = y - clamp(y, f32(j) * c, (f32(j) + 1.0) * c);
        for (var di = 0u; di <= ni; di++) {
            let i = i0 + i32(di);
            let dx = x - clamp(x, f32(i) * c, (f32(i) + 1.0) * c);
            if (dx * dx + dy * dy >= r * r) {
                continue;
            }
            let base = gcell(i, j, salt);
            var prod = 1.0;
            for (var k = 0u; k < GRAIN_MAX_PER_CELL; k++) {
                let h = gpcg(base + k);
                prod *= gunit(h);
                if (prod <= e_end) {
                    break;
                }
                let pos = gpcg(h ^ GRAIN_H2);
                let cx = (f32(i) + gunit16(pos)) * c;
                let cy = (f32(j) + gunit16(pos >> 16u)) * c;
                let ex = x - cx;
                let ey = y - cy;
                if (ex * ex + ey * ey >= r * r) {
                    continue;
                }
                let a = grain_clump_cover(h, ex, ey);
                if (a <= 0.0) {
                    continue;
                }
                var f = 1.0;
                if (prod < e_full) {
                    f = clamp((end + log(prod)) / fw, 0.0, 1.0);
                }
                keep *= 1.0 - f * a;
            }
        }
    }
    return 1.0 - keep;
}

fn grain_gauss(ix: i32, iy: i32, seed: u32) -> f32 {
    let h = gcell(ix, iy, gpcg(seed ^ 0x5bd1e995u));
    let sum = (h & 0xffu) + ((h >> 8u) & 0xffu) + ((h >> 16u) & 0xffu) + (h >> 24u);
    return ((f32(sum) + 2.0) * (1.0 / 256.0) - 2.0) * 1.7320508;
}

fn grain_layer(gx: f32, gy: f32, p: f32, seed: u32) -> f32 {
    var d = 0.0;
    let wf = pf(F_GRAIN_WF);
    let wn = pf(F_GRAIN_WN);
    if (wf > 0.0) {
        let ir = pf(F_GRAIN_INV_R);
        d += wf * (p - grain_field(gx * ir, gy * ir, p, seed));
    }
    if (wn > 0.0) {
        let ppl = pf(F_GRAIN_PPL);
        d -= wn * grain_std(p) * grain_gauss(i32(floor(gx * ppl)), i32(floor(gy * ppl)), seed);
    }
    return d;
}

fn grain_apply(gx: f32, gy: f32, lum: f32) -> vec3<f32> {
    let l = clamp(lum, 0.0, 1.0);
    let gamma = pf(F_GRAIN_GAMMA);
    var shade = max(1.0 - l, 1e-7);
    if (gamma != 1.0) {
        shade = pow(shade, gamma);
    }
    let p = GRAIN_P_MIN + (GRAIN_P_MAX - GRAIN_P_MIN) * shade;
    let seed = pu(F_GRAIN_SEED);
    let tm = 1.0 - shade;
    let amp = pf(F_GRAIN_AMP) * (GRAIN_TONE_FLOOR + (1.0 - GRAIN_TONE_FLOOR) * 4.0 * tm * (1.0 - tm));
    let color = pf(F_GRAIN_COLOR);
    if (color <= 0.0) {
        let lv = amp * grain_layer(gx, gy, p, seed);
        return vec3<f32>(lv, lv, lv);
    }
    // one shared layer and a dye layer per channel: √(1 − c)·mono + √c·dye
    let wm = sqrt(max(1.0 - color, 0.0));
    let wc = sqrt(color);
    let m = wm * amp * grain_layer(gx, gy, p, seed);
    let rr = wc * amp * grain_layer(gx, gy, p, seed ^ GRAIN_SALT_R);
    let gg = wc * amp * grain_layer(gx, gy, p, seed ^ GRAIN_SALT_G);
    let bb = wc * amp * grain_layer(gx, gy, p, seed ^ GRAIN_SALT_B);
    return vec3<f32>(m + rr, m + gg, m + bb);
}

fn enc8(v: f32) -> u32 {
    return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}

// sRGB-curve-encoded value → the output space's own curve (`OutputTrc`: 0 sRGB, 1 gamma, 2 Rec.709).
fn out_encode(v: f32) -> f32 {
    let kind = pu(F_OUT_TRC);
    if (kind == 0u) {
        return v;
    }
    let e = clamp(v, 0.0, 1.0);
    var l = e / 12.92;
    if (e > 0.04045) {
        l = pow((e + 0.055) / 1.055, 2.4);
    }
    if (kind == 1u) {
        return pow(l, 1.0 / pf(F_OUT_GAMMA));
    }
    if (l < 0.018) {
        return l * 4.5;
    }
    return 1.099 * pow(l, 0.45) - 0.099;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let w = pu(F_W);
    let h = pu(F_H);
    let x = gid.x;
    let y = gid.y + pu(F_Y0);
    if (x >= w || y >= h) {
        return;
    }
    let i = y * w + x;
    let n = w * h;
    let gain = pf(F_GAIN);
    let ev = pf(F_EV);
    var c = vec3<f32>(img[3u * i], img[3u * i + 1u], img[3u * i + 2u]);
    if (gain != 1.0) {
        c = c * gain;
    }
    let l_pre = log_l[i];
    let l0 = l_pre + ev;

    // --- local (mask) contributions
    var lt: array<f32, MASK_SUMS>;
    var tint_on = false;
    var tint_dir = vec2<f32>(0.0, 0.0);
    var tint_amt = 0.0;
    let nm = pu(F_NMASK);
    for (var m = 0u; m < nm; m++) {
        let a = masks[m * n + i];
        if (a <= 0.0) {
            continue;
        }
        let t = pu(F_MASK_OFF) + m * MASK_TERMS;
        for (var k = 0u; k < MASK_SUMS; k++) {
            lt[k] += a * aux[t + k];
        }
        if (aux[t + MASK_SUMS] > 0.0) {
            tint_on = true;
            tint_dir = vec2<f32>(aux[t + MASK_SUMS + 1u], aux[t + MASK_SUMS + 2u]);
            tint_amt = a * aux[t + MASK_SUMS + 3u];
        }
    }
    let l_exp = lt[0];
    let l_temp = lt[1];
    let l_tint = lt[2];
    let l_noise = lt[14];

    // --- local Moiré (and the colour part of Noise): chromaticity towards its blur
    let mt = clamp(lt[15] + 0.5 * max(l_noise, 0.0), -1.0, 1.0);
    if (mt != 0.0 && pu(F_HAS_CHROMA) != 0u) {
        let raw = vec3<f32>(img[3u * i], img[3u * i + 1u], img[3u * i + 2u]);
        let y = lum2020(c);
        let ch0 = raw / max(lum2020(raw), 1e-6);
        let o = nm * n + 3u * i;
        let cb = vec3<f32>(masks[o], masks[o + 1u], masks[o + 2u]);
        c = max((ch0 + (cb - ch0) * mt) * y, vec3<f32>(0.0));
    }
    // --- local Defringe: desaturate purple / green fringes along edges
    let df = clamp(lt[16], 0.0, 1.0);
    if (df > 0.0 && pu(F_HAS_TEX) != 0u) {
        let yd = max(lum2020(c), 1e-6);
        let purple = (min(c.x, c.z) - c.y) / yd;
        let green = (c.y - max(c.x, c.z)) / yd;
        let k = df * sstep(0.04, 0.3, abs(l_pre - tex[i])) * max(sstep(0.02, 0.2, purple), sstep(0.02, 0.2, green));
        if (k > 0.0) {
            let y = lum2020(c);
            c = c + (y - c) * k;
        }
    }

    // --- dehaze (scene linear)
    let dz = pf(F_DEHAZE) + lt[10];
    if (dz != 0.0 && pu(F_HAS_DARK) != 0u) {
        let d = clamp(dark[i] / pf(F_AIR_PRE), 0.0, 1.0);
        let air = pf(F_AIR);
        if (dz > 0.0) {
            let t = max(1.0 - 0.95 * min(dz, 1.0) * d, 0.12);
            c = max((c - air * (1.0 - t)) / t, vec3<f32>(0.0));
        } else {
            let k = min(-dz, 1.0) * 0.7 * (0.35 + 0.65 * d);
            c = c + (air * 0.9 - c) * k;
        }
    }

    // --- local exposure / temp / tint
    if (l_exp != 0.0) {
        c = c * exp2(l_exp);
    }
    if (l_temp != 0.0 || l_tint != 0.0) {
        let y0 = lum2020(c);
        c = vec3<f32>(c.x * (1.0 + 0.3 * l_temp), c.y * (1.0 - 0.22 * l_tint), c.z * max(1.0 - 0.3 * l_temp, 0.0));
        let y1 = max(lum2020(c), 1e-9);
        c = c * y0 / y1;
    }

    // --- local tone in log luminance
    var l1 = l0;
    if (dz != 0.0 || l_exp != 0.0) {
        l1 = log_lum(c);
    }
    let shift = l1 - l0;
    let base = base_p[i] + ev + shift;
    var delta = 0.0;
    let hh = pf(F_HL) + lt[4];
    let ss = pf(F_SH) + lt[5];
    if (hh != 0.0 || ss != 0.0) {
        let ws = 1.0 - sstep(-4.8, 0.3, base);
        let wh = sstep(-1.0, 2.8, base);
        delta += ss * 1.7 * ws * sqrt(ws) + hh * 1.7 * wh;
    }
    if (lt[6] != 0.0) {
        delta += lt[6] * 0.8 * sstep(0.5, 3.0, l1);
    }
    if (lt[7] != 0.0) {
        delta += lt[7] * 0.8 * (1.0 - sstep(-6.0, -1.5, l1));
    }
    if (lt[3] != 0.0) {
        delta += lt[3] * 0.14 * clamp(l1, -6.0, 4.0);
    }
    let cl = pf(F_CLAR) + lt[9];
    if (cl != 0.0 && pu(F_HAS_CLAR) != 0u) {
        let det = clamp(l_pre - clar[i], -2.5, 2.5);
        let q = base / 3.2;
        let mid = exp(-(q * q));
        delta += cl * 0.85 * det * (0.35 + 0.65 * mid);
    }
    let tx = pf(F_TEX) + lt[8];
    let sp = lt[13] * 0.6 + pf(F_SHARPEN);
    if ((tx != 0.0 || sp != 0.0) && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        let tame = 1.0 - 0.6 * sstep(0.4, 1.6, abs(det));
        delta += tx * 1.1 * clamp(det, -1.0, 1.0) * tame;
        if (sp != 0.0) {
            let sm = pf(F_SHARPEN_MASK);
            var mk = 1.0;
            if (sm > 0.0) {
                mk = sstep(sm * 0.25, sm * 0.25 + 0.15, abs(det));
            }
            delta += sp * 1.3 * clamp(det, -0.8, 0.8) * mk;
        }
    }
    // local Noise: smooth (or, negative, boost) small-amplitude detail, keep edges
    if (l_noise != 0.0 && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        delta -= clamp(l_noise, -1.0, 1.0) * 0.9 * det * (1.0 - sstep(0.1, 0.5, abs(det)));
    }
    if (delta != 0.0) {
        c = c * exp2(delta);
    }

    // --- calibration (scene linear, before the tone map)
    if (pu(F_CALIB) != 0u || pf(F_SHADOW_TINT) != 0.0) {
        c = calibrate(c);
    }

    // --- tone map on luminance, highlight desaturation
    let yl = lum2020(c);
    let o = tone_apply(yl);
    var d = vec3<f32>(0.0);
    if (yl > 1e-9) {
        d = c * o / yl;
    }
    let k = chroma_scale(o);
    if (k != 1.0) {
        d = vec3<f32>(o) + (d - vec3<f32>(o)) * k;
    }
    let mx = max(d.x, max(d.y, d.z));
    if (mx > 1.0) {
        let t = clamp((mx - 1.0) / max(mx - o, 1e-6), 0.0, 1.0);
        d = d + (o - d) * t;
    }

    // --- colour
    d = color_ops(d, lt[11], lt[12]);
    if (tint_on) {
        let lab = oklab(d);
        d = oklab_inv(vec3<f32>(lab.x, lab.y + tint_dir.x * 0.08 * tint_amt, lab.z + tint_dir.y * 0.08 * tint_amt));
    }

    // --- vignette (display linear, post-crop)
    if (pu(F_VIG) != 0u) {
        // the vignette belongs to the whole output frame, not to the window being drawn
        let fw = pf(F_VIG_VIEW + 2u);
        let fh = pf(F_VIG_VIEW + 3u);
        let aspect = fw / fh;
        let amount = pf(F_VIG_AMOUNT);
        let mixa = pf(F_VIG_ASPECT_MIX);
        let power = pf(F_VIG_POWER);
        let u = (pf(F_VIG_VIEW) + f32(x) + 0.5) / fw * 2.0 - 1.0;
        let vv = (pf(F_VIG_VIEW + 1u) + f32(y) + 0.5) / fh * 2.0 - 1.0;
        let sx = 1.0 + (aspect - 1.0) * mixa;
        let sy = 1.0 + (1.0 / aspect - 1.0) * mixa;
        let ax = abs(u * max(sx, 1.0) / max(sx, sy));
        let ay = abs(vv * max(sy, 1.0) / max(sx, sy));
        let dist = pow(pow(ax, power) + pow(ay, power), 1.0 / power);
        let start = pf(F_VIG_START);
        let t = sstep(start, start + pf(F_VIG_WIDTH), dist);
        if (t > 0.0) {
            let lum = clamp(lum2020(d), 0.0, 1.0);
            if (amount < 0.0) {
                var f = 1.0 + amount * t;
                let style = pu(F_VIG_STYLE);
                if (style == 1u) {
                    f += (1.0 - f) * pf(F_VIG_HL) * sstep(0.4, 1.0, lum);
                }
                if (style == 2u) {
                    d = d * (1.0 - (-amount) * t) + 0.0;
                } else {
                    d = d * f;
                }
            } else {
                d = d + (1.0 - d) * amount * t * 0.85;
            }
        }
    }

    // --- gamut map to the output space (desaturate towards luminance until in range)
    let om = array<vec3<f32>, 3>(
        vec3<f32>(pf(F_OUT_M), pf(F_OUT_M + 1u), pf(F_OUT_M + 2u)),
        vec3<f32>(pf(F_OUT_M + 3u), pf(F_OUT_M + 4u), pf(F_OUT_M + 5u)),
        vec3<f32>(pf(F_OUT_M + 6u), pf(F_OUT_M + 7u), pf(F_OUT_M + 8u)),
    );
    var r = mul3(om, d);
    let yy = clamp(pf(F_OUT_Y) * r.x + pf(F_OUT_Y + 1u) * r.y + pf(F_OUT_Y + 2u) * r.z, 0.0, 1.0);
    var tg = 1.0;
    for (var k = 0u; k < 3u; k++) {
        let cc = r[k];
        if (cc < 0.0) {
            tg = min(tg, yy / max(yy - cc, 1e-9));
        } else if (cc > 1.0) {
            tg = min(tg, (1.0 - yy) / max(cc - yy, 1e-9));
        }
    }
    if (tg < 1.0) {
        r = yy + (r - yy) * tg;
    }

    // --- encode, curves, grain
    var e = vec3<f32>(encode_srgb(r.x), encode_srgb(r.y), encode_srgb(r.z));
    if (pu(F_CURVES) != 0u) {
        let e0 = e;
        e = vec3<f32>(curve(0u, e.x), curve(1u, e.y), curve(2u, e.z));
        let rs = pf(F_REFINE_SAT);
        if (rs < 1.0) {
            e = refine_saturation(e0, e, rs);
        }
    }
    if (pu(F_GRAIN) != 0u) {
        let px = f32(x) + 0.5;
        let py = f32(y) + 0.5;
        let gx = pf(F_GRAIN_AFF) * px + pf(F_GRAIN_AFF + 2u) * py + pf(F_GRAIN_AFF + 4u);
        let gy = pf(F_GRAIN_AFF + 1u) * px + pf(F_GRAIN_AFF + 3u) * py + pf(F_GRAIN_AFF + 5u);
        let lum = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
        e = e + grain_apply(gx, gy, lum);
    }
    e = vec3<f32>(out_encode(e.x), out_encode(e.y), out_encode(e.z));
    out[i] = enc8(e.x) | (enc8(e.y) << 8u) | (enc8(e.z) << 16u) | (255u << 24u);
}
