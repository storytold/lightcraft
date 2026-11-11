// The per-pixel stage: a straight port of `lightcraft_pipeline::finish` (keep in step with it).
// Bindings: img (rgb, pre-exposure), log_l, base, clar, tex, dark, masks (NMASK planes, then the
// blurred chromaticity when HAS_CHROMA), aux
// (tone LUT | chroma curve | sRGB LUT | curve LUTs | mask terms | light operator tables), out (packed RGBA8; a float render:
// three words per pixel, see `F_DEEP`).

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

// `linear_to_srgb`: the exact sRGB curve, unbounded above 1 (a float render encodes with it).
fn srgb_exact(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

// `srgb_to_linear`.
fn srgb_linear(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

// Above SDR white (an HDR render) the curves continue with slope 1 from their end point.
fn curve(ch: u32, x: f32) -> f32 {
    let o = pu(F_CURVE_OFF) + ch * CURVE_N;
    let f = clamp(x, 0.0, 1.0) * f32(CURVE_N - 1u);
    let i = min(u32(f), CURVE_N - 2u);
    let t = f - f32(i);
    let v = aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
    if (x > 1.0 && pf(F_PEAK) > 1.0) {
        return v + (x - 1.0);
    }
    return v;
}

// Process V4's calibrated Contrast, Blacks (`lightcraft_pipeline::light_v3`): operator `k`'s
// table (linear output over encoded v) at v (`Operator::eval`).
fn light_eval(k: u32, v: f32) -> f32 {
    let o = pu(F_LIGHT_OFF) + k * LIGHT_N;
    let f = clamp(v, 0.0, 1.0) * f32(LIGHT_N - 1u);
    let i = min(u32(f), LIGHT_N - 2u);
    let t = f - f32(i);
    return aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
}

// `Operator::channel`: one linear value; above 1 unchanged, below 0 shifted by F(0).
fn light_channel(k: u32, x: f32) -> f32 {
    if (x > 1.0) {
        return x;
    }
    if (x < 0.0) {
        return aux[pu(F_LIGHT_OFF) + k * LIGHT_N] + x;
    }
    if (pu(F_DEEP) != 0u) {
        return light_eval(k, srgb_exact(x));
    }
    return light_eval(k, encode_srgb(x));
}

// `FrameRange::highlights` (process V9): Highlights scaled by the frame's tonal range HS_RANGE_LO..HS_RANGE_HI.
fn hs_range_highlights(hh: f32, base: f32) -> f32 {
    if (hh == 0.0) {
        return 0.0;
    }
    let span = max(pf(F_HS_RANGE_HI) - pf(F_HS_RANGE_LO), 0.1);
    let amp = clamp(HS_H_AMP * pow(span / HS_H_SPAN, HS_H_POW), HS_H_AMP_MIN, HS_H_AMP_MAX);
    let low = -1.0 - HS_H_REACH * max(span - HS_H_SPAN, 0.0);
    return hh * amp * sstep(low, 2.8, base);
}

// `default_tone::rgb_tone`: the base tone on the largest and smallest linear ProPhoto channel, the middle
// channel keeping its place between them (process V9 with Lightroom's default curve).
fn rgb_tone(c: vec3<f32>) -> vec3<f32> {
    let p = max(mul3(PRO_FROM_2020, c), vec3<f32>(0.0));
    let a = max(p.x, max(p.y, p.z));
    let b = min(p.x, min(p.y, p.z));
    let fa = tone_apply(a);
    let fb = tone_apply(b);
    var o = vec3<f32>(fa);
    if (a > b) {
        o = vec3<f32>(fb) + vec3<f32>(fa - fb) * (p - vec3<f32>(b)) / (a - b);
    }
    return mul3(PRO_TO_2020, o);
}

// `Operator::luminance`: scaled by F of the ProPhoto luminance over it.
fn light_lum(k: u32, p: vec3<f32>) -> vec3<f32> {
    let y = PRO_Y.x * p.x + PRO_Y.y * p.y + PRO_Y.z * p.z;
    if (!(y > 1e-6) || y > 1.0) {
        return p;
    }
    return p * (light_channel(k, y) / y);
}

// `LightOps::apply`: linear Rec.2020 → ProPhoto, the operators in order, back.
fn light_ops(d: vec3<f32>) -> vec3<f32> {
    var p = mul3(PRO_FROM_2020, d);
    let n = min(pu(F_LIGHT_OPS), 2u);
    for (var k = 0u; k < n; k++) {
        let mode = pu(F_LIGHT_MODE + k);
        if (mode == 0u) {
            p = vec3<f32>(light_channel(k, p.x), light_channel(k, p.y), light_channel(k, p.z));
        } else if (mode == 1u) {
            p = light_lum(k, p);
        } else {
            let a = vec3<f32>(light_channel(k, p.x), light_channel(k, p.y), light_channel(k, p.z));
            let b = light_lum(k, p);
            let w = pf(F_LIGHT_W + k);
            p = vec3<f32>(w * a.x + (1.0 - w) * b.x, w * a.y + (1.0 - w) * b.y, w * a.z + (1.0 - w) * b.z);
        }
    }
    return mul3(PRO_TO_2020, p);
}

// `mixer_stage::level`: the lower and upper of three keys around `x` and the upper's weight, clamped.
struct CLevel {
    a: u32,
    b: u32,
    f: f32,
}

fn mstage_level(k: vec3<f32>, x: f32) -> CLevel {
    if (x <= k.x) {
        return CLevel(0u, 0u, 0.0);
    }
    if (x <= k.y) {
        return CLevel(0u, 1u, (x - k.x) / (k.y - k.x));
    }
    if (x <= k.z) {
        return CLevel(1u, 2u, (x - k.y) / (k.z - k.y));
    }
    return CLevel(2u, 2u, 0.0);
}

// `MixerStage::look`, with the level keys at `keys` (9 chroma by lightness level, then 3 lightness) and the table
// after them: (hue shift, chroma factor, lightness change).
fn mstage_look(keys: u32, l: f32, c: f32, h: f32) -> vec3<f32> {
    let t = keys + 12u;
    let hd = ((h * 57.29577951308232) % 360.0 + 360.0) % 360.0 / 5.0;
    let i0 = u32(hd) % 72u;
    let i1 = (i0 + 1u) % 72u;
    let fh = hd - floor(hd);
    let lb = mstage_level(vec3<f32>(aux[keys + 9u], aux[keys + 10u], aux[keys + 11u]), l);
    var r = array<vec3<f32>, 2>(vec3<f32>(0.0), vec3<f32>(0.0));
    for (var j = 0u; j < 2u; j++) {
        let m = select(lb.a, lb.b, j == 1u);
        let ck = vec3<f32>(aux[keys + 3u * m], aux[keys + 3u * m + 1u], aux[keys + 3u * m + 2u]);
        let cb = mstage_level(ck, c);
        var v = vec3<f32>(0.0);
        for (var o = 0u; o < 3u; o++) {
            let r0 = t + ((m * 3u + cb.a) * 3u + o) * 72u;
            let r1 = t + ((m * 3u + cb.b) * 3u + o) * 72u;
            let a = aux[r0 + i0] * (1.0 - fh) + aux[r0 + i1] * fh;
            let b = aux[r1 + i0] * (1.0 - fh) + aux[r1 + i1] * fh;
            v[o] = a * (1.0 - cb.f) + b * cb.f;
        }
        if (c < ck.x) {
            v.z *= max(c / ck.x, 0.0);
        }
        r[j] = v;
    }
    return r[0] * (1.0 - lb.f) + r[1] * lb.f;
}

// `MixerStage::apply`: the colour mixer, through OkLCh.
fn mixer_stage(d: vec3<f32>) -> vec3<f32> {
    let lab = oklab(d);
    let l = lab.x;
    let c = sqrt(lab.y * lab.y + lab.z * lab.z);
    // atan2(0, 0) is undefined in WGSL; the CPU's gives 0
    let h = select(atan2(lab.z, lab.y), 0.0, c == 0.0);
    let r = mstage_look(pu(F_MSTAGE_OFF), l, c, h);
    let h2 = h + r.x;
    let c2 = c * r.y;
    return oklab_inv(vec3<f32>(max(l + r.z, 0.0), c2 * cos(h2), c2 * sin(h2)));
}

// A table of process V9's curve stage (`Lut1::eval`) at offset `o` in `aux`.
fn stage_lut(o: u32, x: f32) -> f32 {
    let f = clamp(x, 0.0, 1.0) * f32(CURVE_N - 1u);
    let i = min(u32(f), CURVE_N - 2u);
    let t = f - f32(i);
    return aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
}

// `CurveStage::apply`: the base table on the largest and smallest channel of sRGB-encoded linear ProPhoto, the
// middle one keeping its place between them in linear light, then the red, green and blue curves per channel.
fn curve_stage(d: vec3<f32>) -> vec3<f32> {
    let p = mul3(PRO_FROM_2020, d);
    var pc = clamp(p, vec3<f32>(0.0), vec3<f32>(1.0));
    var e = vec3<f32>(srgb_exact(pc.x), srgb_exact(pc.y), srgb_exact(pc.z));
    var hi = 0u;
    var lo = 0u;
    for (var k = 1u; k < 3u; k++) {
        if (e[k] > e[hi]) {
            hi = k;
        }
        if (e[k] < e[lo]) {
            lo = k;
        }
    }
    let off = pu(F_STAGE_OFF);
    let base = off + 3u * CURVE_N;
    let la = srgb_linear(stage_lut(base, e[hi]));
    let lb = srgb_linear(stage_lut(base, e[lo]));
    let pa = pc[hi];
    let pb = pc[lo];
    var curved = vec3<f32>(0.0);
    for (var k = 0u; k < 3u; k++) {
        var t = 1.0;
        if (pa > pb) {
            t = (pc[k] - pb) / (pa - pb);
        }
        curved[k] = stage_lut(off + k * CURVE_N, srgb_exact(clamp(lb + (la - lb) * t, 0.0, 1.0)));
    }
    var q = vec3<f32>(0.0);
    for (var k = 0u; k < 3u; k++) {
        q[k] = srgb_linear(clamp(curved[k], 0.0, 1.0));
    }
    let rs = pf(F_STAGE_REFINE);
    if (rs < 1.0) {
        // the saturation back towards the input's, at the curves' luminance
        let rr = refine_saturation(e, curved, rs);
        var r = vec3<f32>(0.0);
        for (var k = 0u; k < 3u; k++) {
            r[k] = srgb_linear(clamp(rr[k], 0.0, 1.0));
        }
        let yq = dot(q, PRO_Y);
        let yr = dot(r, PRO_Y);
        q = r;
        if (yr > 1e-6) {
            q = r * (yq / yr);
        }
    }
    for (var k = 0u; k < 3u; k++) {
        if (p[k] > 1.0) {
            q[k] += p[k] - 1.0;
        }
    }
    return mul3(PRO_TO_2020, q);
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
    // a neutral (black or grey) pixel has no hue: atan2(0, 0) is undefined in WGSL (NaN on some
    // GPUs, which a later lightening such as the vignette then shows); the CPU's atan2 gives 0
    var h = select(atan2(lab0.z, lab0.y), 0.0, c == 0.0);
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
    if (pu(F_MGRADE) != 0u) {
        lab = measured_grade(lab);
    }
    return oklab_inv(lab);
}

// `grading_stage` lightness grid (65 entries at `o`), clamped.
fn mgrade_grid(o: u32, l: f32) -> f32 {
    let p = clamp(l, 0.0, 1.0) * 64.0;
    let i = min(u32(p), 63u);
    return aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * (p - f32(i));
}

// `GradingStage::apply`: each wheel's damped tint by its lightness profile, then the Luminance curve.
fn measured_grade(lab: vec3<f32>) -> vec3<f32> {
    let off = pu(F_MGRADE_OFF);
    let n = pu(F_MGRADE) - 1u;
    let c = sqrt(lab.y * lab.y + lab.z * lab.z);
    // atan2(0, 0) is undefined in WGSL; the CPU's gives 0
    let hue = select(atan2(lab.z, lab.y), 0.0, c == 0.0);
    var out = lab;
    for (var k = 0u; k < n; k++) {
        let w = off + 65u + k * 85u;
        let p = mgrade_grid(w + 20u, lab.x);
        if (p == 0.0) {
            continue;
        }
        let ta = aux[w];
        let tb = aux[w + 1u];
        let diff = abs(wrap_angle(hue - atan2(tb, ta)));
        let bin = min(u32(diff * 57.29577951308232 / 30.0), 5u);
        // the damping's chroma levels (`grading_stage_tables::CHROMA`)
        var damp = 1.0;
        if (c <= 0.04) {
            damp = 1.0 + (aux[w + 2u + bin] - 1.0) * c / 0.04;
        } else if (c <= 0.08) {
            let f = (c - 0.04) / 0.04;
            damp = aux[w + 2u + bin] * (1.0 - f) + aux[w + 8u + bin] * f;
        } else if (c <= 0.12) {
            let f = (c - 0.08) / 0.04;
            damp = aux[w + 8u + bin] * (1.0 - f) + aux[w + 14u + bin] * f;
        } else {
            damp = aux[w + 14u + bin];
        }
        out.y += p * ta * damp;
        out.z += p * tb * damp;
    }
    out.x = max(lab.x + mgrade_grid(off, lab.x), 0.0);
    return out;
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

fn ghash(i: i32, j: i32, seed: u32) -> f32 {
    var v = (bitcast<u32>(i) * GRAIN_H0) ^ (bitcast<u32>(j) * GRAIN_H1) ^ (seed * GRAIN_H2);
    v ^= v >> 13u;
    v = v * GRAIN_H3;
    v ^= v >> 15u;
    return f32(v & 0xffffu) / 32768.0 - 1.0;
}

// Crystal grain (process V4): `lightcraft_pipeline::crystal` (keep in step with it). The tori's
// summed-area tables follow the other tables in `aux` (offsets F_GRAIN_SAT).
fn cg_bilinear(o: u32, t: u32, x: f32, y: f32) -> f32 {
    let s = t + 1u;
    let i = min(u32(max(x, 0.0)), t - 1u);
    let j = min(u32(max(y, 0.0)), t - 1u);
    let fx = x - f32(i);
    let fy = y - f32(j);
    let b = o + j * s + i;
    let a = aux[b] + (aux[b + 1u] - aux[b]) * fx;
    let c = aux[b + s] + (aux[b + s + 1u] - aux[b + s]) * fx;
    return a + (c - a) * fy;
}

fn cg_integral(o: u32, t: u32, x: f32, y: f32) -> f32 {
    let tf = f32(t);
    let wx = x >= tf;
    let wy = y >= tf;
    let rx = select(x, x - tf, wx);
    let ry = select(y, y - tf, wy);
    var v = cg_bilinear(o, t, rx, ry);
    if (wx) {
        v += cg_bilinear(o, t, tf, ry);
    }
    if (wy) {
        v += cg_bilinear(o, t, rx, tf);
    }
    if (wx && wy) {
        v += cg_bilinear(o, t, tf, tf);
    }
    return v;
}

fn cg_box(o: u32, t: u32, x0_: f32, y0_: f32, w: f32) -> f32 {
    let tf = f32(t);
    let x0 = x0_ - floor(x0_ / tf) * tf;
    let y0 = y0_ - floor(y0_ / tf) * tf;
    return cg_integral(o, t, x0 + w, y0 + w) - cg_integral(o, t, x0, y0 + w) - cg_integral(o, t, x0 + w, y0) + cg_integral(o, t, x0, y0);
}

fn cg_lift(v: f32) -> f32 {
    let c = clamp(v, 0.0, 1.0);
    return select((c + 0.055) / 2.4, c, c <= 0.04045);
}

fn cg_srgb_to_linear(v: f32) -> f32 {
    return select(pow((v + 0.055) / 1.055, 2.4), v / 12.92, v <= 0.04045);
}

fn crystal_grain(e: vec3<f32>, gx: f32, gy: f32) -> vec3<f32> {
    let w = pf(F_GRAIN_FOOT);
    let x0 = gx * CRYSTAL_TPL - 0.5 * w;
    let y0 = gy * CRYSTAL_TPL - 0.5 * w;
    let g = (cg_box(pu(F_GRAIN_SAT), CRYSTAL_T0, x0, y0, w) + cg_box(pu(F_GRAIN_SAT + 1u), CRYSTAL_T1, x0, y0, w)) / (w * w);
    let lum = clamp(0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z, 0.0, 1.0);
    let k = pf(F_GRAIN_K) * g * (1.0 - cg_srgb_to_linear(lum));
    return e + k * vec3<f32>(cg_lift(e.x), cg_lift(e.y), cg_lift(e.z));
}

fn grain_noise(x: f32, y: f32, seed: u32) -> f32 {
    let x0 = floor(x);
    let y0 = floor(y);
    let fx = x - x0;
    let fy = y - y0;
    let i = i32(x0);
    let j = i32(y0);
    let u = fx * fx * (3.0 - 2.0 * fx);
    let v = fy * fy * (3.0 - 2.0 * fy);
    let a = ghash(i, j, seed) + (ghash(i + 1, j, seed) - ghash(i, j, seed)) * u;
    let b = ghash(i, j + 1, seed) + (ghash(i + 1, j + 1, seed) - ghash(i, j + 1, seed)) * u;
    return a + (b - a) * v;
}

// The 3×3 matrix at parameter `o` (row-major).
fn mat_at(o: u32) -> array<vec3<f32>, 3> {
    return array<vec3<f32>, 3>(
        vec3<f32>(pf(o), pf(o + 1u), pf(o + 2u)),
        vec3<f32>(pf(o + 3u), pf(o + 4u), pf(o + 5u)),
        vec3<f32>(pf(o + 6u), pf(o + 7u), pf(o + 8u)),
    );
}

// `finish::out_of_gamut`: needed real desaturation, and isn't a neutral clipping at white/black.
fn out_of_gamut(r: vec3<f32>, t: f32) -> bool {
    let lo = min(r.x, min(r.y, r.z));
    let hi = max(r.x, max(r.y, r.z));
    return t < GAMUT_WARN && (hi - lo) > 0.02 * max(abs(hi), 1e-3) && hi - lo > 1e-3 && (lo < -1e-3 || (hi > 1.0 && lo < 1.0));
}

// `Lut3d::apply` (trilinear) on the LUT profile's table in `aux` (red fastest).
fn lut_at(r: u32, g: u32, b: u32) -> vec3<f32> {
    let n = pu(F_LUT_N);
    let o = pu(F_LUT_OFF) + 3u * (r + n * (g + n * b));
    return vec3<f32>(aux[o], aux[o + 1u], aux[o + 2u]);
}

fn lut_apply(c: vec3<f32>) -> vec3<f32> {
    let n = pu(F_LUT_N);
    let s = f32(n - 1u);
    var idx = vec3<u32>(0u);
    var fr = vec3<f32>(0.0);
    for (var k = 0u; k < 3u; k++) {
        let lo = pf(F_LUT_MIN + k);
        let span = max(pf(F_LUT_MAX + k) - lo, 1e-6);
        let x = clamp((c[k] - lo) / span, 0.0, 1.0) * s;
        let i = min(u32(floor(x)), n - 2u);
        idx[k] = i;
        fr[k] = x - f32(i);
    }
    let r = idx.x;
    let g = idx.y;
    let b = idx.z;
    let c00 = mix(lut_at(r, g, b), lut_at(r + 1u, g, b), fr.x);
    let c10 = mix(lut_at(r, g + 1u, b), lut_at(r + 1u, g + 1u, b), fr.x);
    let c01 = mix(lut_at(r, g, b + 1u), lut_at(r + 1u, g, b + 1u), fr.x);
    let c11 = mix(lut_at(r, g + 1u, b + 1u), lut_at(r + 1u, g + 1u, b + 1u), fr.x);
    return mix(mix(c00, c10, fr.y), mix(c01, c11, fr.y), fr.z);
}

const BAYER_8X8 = array<f32, 64>(
     0.0, 32.0,  8.0, 40.0,  2.0, 34.0, 10.0, 42.0,
    48.0, 16.0, 56.0, 24.0, 50.0, 18.0, 58.0, 26.0,
    12.0, 44.0,  4.0, 36.0, 14.0, 46.0,  6.0, 38.0,
    60.0, 28.0, 52.0, 20.0, 62.0, 30.0, 54.0, 22.0,
     3.0, 35.0, 11.0, 43.0,  1.0, 33.0,  9.0, 41.0,
    51.0, 19.0, 59.0, 27.0, 49.0, 17.0, 57.0, 25.0,
    15.0, 47.0,  7.0, 39.0, 13.0, 45.0,  5.0, 37.0,
    63.0, 31.0, 55.0, 23.0, 61.0, 29.0, 53.0, 21.0,
);

fn enc8(v: f32, d: f32) -> u32 {
    let y = clamp(v, 0.0, 1.0) * 255.0;
    if (y >= 254.9) {
        return 255u;
    }
    return u32(clamp(y + d, 0.0, 255.0));
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

// `finish::gamut_map_to`: the chroma scale that brings `r` into 0..top towards its luminance (1: in gamut) ...
fn gamut_scale(r: vec3<f32>, luma: vec3<f32>, top: f32) -> f32 {
    let yy = clamp(dot(luma, r), 0.0, top);
    var tg = 1.0;
    for (var k = 0u; k < 3u; k++) {
        let cc = r[k];
        if (cc < 0.0) {
            tg = min(tg, yy / max(yy - cc, 1e-9));
        } else if (cc > top) {
            tg = min(tg, (top - yy) / max(cc - yy, 1e-9));
        }
    }
    return tg;
}

// ... and `r` scaled by it.
fn gamut_apply(r: vec3<f32>, luma: vec3<f32>, tg: f32, top: f32) -> vec3<f32> {
    if (tg < 1.0) {
        let yy = clamp(dot(luma, r), 0.0, top);
        return yy + (r - yy) * tg;
    }
    return r;
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
    let peak = pf(F_PEAK);
    let hdr = peak > 1.0;
    let deep = pu(F_DEEP) != 0u;
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
        // the dark channel is a blur, not a patch minimum: never attribute more veil than the
        // pixel's own minimum channel, or dark pixels next to bright ones crush to black (#2504)
        let air = pf(F_AIR);
        let cmin = min(c.x, min(c.y, c.z));
        let d = clamp(min(select(0.0, dark[i] / pf(F_AIR_PRE), pf(F_AIR_PRE) > 0.0), select(0.0, cmin / air, air > 0.0)), 0.0, 1.0);
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
        delta += ss * 1.7 * ws * sqrt(ws);
        if (pu(F_HS_RANGE) != 0u) {
            delta += hs_range_highlights(hh, base);
        } else {
            delta += hh * 1.7 * sstep(-1.0, 2.8, base);
        }
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
    if (tx != 0.0 && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        let tame = 1.0 - 0.6 * sstep(0.4, 1.6, abs(det));
        delta += tx * 1.1 * clamp(det, -1.0, 1.0) * tame;
    }
    let sp = lt[13] * 0.6 + pf(F_SHARPEN);
    if (pu(F_SHARPEN_V3) != 0u && sp != 0.0 && pu(F_HAS_SHP) != 0u) {
        let det = l_pre - tex[pu(F_SHP_OFF) + i];
        let sm = pf(F_SHARPEN_MASK);
        var mk = 1.0;
        if (sm > 0.0) {
            mk = sstep(sm * 0.1, sm * 0.1 + 0.06, abs(det));
        }
        delta += sp * pf(F_SHARPEN_GAIN) * 7.0 * mk * det / (1.0 + abs(det) / pf(F_SHARPEN_LIMIT));
    } else if (pu(F_SHARPEN_V3) == 0u && sp != 0.0 && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        let sm = pf(F_SHARPEN_MASK);
        var mk = 1.0;
        if (sm > 0.0) {
            mk = sstep(sm * 0.25, sm * 0.25 + 0.15, abs(det));
        }
        delta += sp * 1.3 * clamp(det, -0.8, 0.8) * mk;
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
    if (pu(F_RGB_TONE) != 0u) {
        d = rgb_tone(c);
    } else if (yl > 1e-9) {
        d = c * o / yl;
    }
    let k = chroma_scale(o);
    if (k != 1.0) {
        d = vec3<f32>(o) + (d - vec3<f32>(o)) * k;
    }
    let mx = max(d.x, max(d.y, d.z));
    if (mx > peak) {
        let t = clamp((mx - peak) / max(mx - o, 1e-6), 0.0, 1.0);
        d = d + (o - d) * t;
    }

    // --- process V4: calibrated Contrast, Blacks
    if (pu(F_LIGHT_OPS) != 0u) {
        d = light_ops(d);
    }

    // --- process V9: the colour mixer, then Whites and the tone curves
    if (pu(F_MSTAGE) != 0u) {
        d = mixer_stage(d);
    }
    if (pu(F_STAGE) != 0u) {
        d = curve_stage(d);
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
            } else if (hdr) {
                // lighten towards SDR white; never pull HDR highlights down to it
                d = d + max(1.0 - d, vec3<f32>(0.0)) * amount * t * 0.85;
            } else {
                d = d + (1.0 - d) * amount * t * 0.85;
            }
        }
    }

    // --- gamut map to the output space (desaturate towards luminance until in range);
    // soft proofing maps into the proof space first and shows that in the output space
    // (warn: 0 none, 1 destination gamut, 2 display gamut)
    var warn = 0u;
    var r: vec3<f32>;
    if (pu(F_PROOF) != 0u) {
        let q0 = mul3(mat_at(F_PROOF_M), d);
        let tq = gamut_scale(q0, vec3<f32>(pf(F_PROOF_Y), pf(F_PROOF_Y + 1u), pf(F_PROOF_Y + 2u)), peak);
        if (pu(F_PROOF_DEST_WARN) != 0u && out_of_gamut(q0, tq)) {
            warn = 1u;
        }
        r = mul3(mat_at(F_PROOF_OUT), gamut_apply(q0, vec3<f32>(pf(F_PROOF_Y), pf(F_PROOF_Y + 1u), pf(F_PROOF_Y + 2u)), tq, peak));
    } else {
        r = mul3(mat_at(F_OUT_M), d);
    }
    let oy = vec3<f32>(pf(F_OUT_Y), pf(F_OUT_Y + 1u), pf(F_OUT_Y + 2u));
    let tg = gamut_scale(r, oy, peak);
    if (pu(F_PROOF) != 0u && pu(F_PROOF_DISP_WARN) != 0u && warn == 0u && out_of_gamut(r, tg)) {
        warn = 2u;
    }
    r = gamut_apply(r, oy, tg, peak);

    // --- encode, curves, grain
    var e = vec3<f32>(encode_srgb(r.x), encode_srgb(r.y), encode_srgb(r.z));
    if (deep) {
        // the exact curve; an HDR render's values above SDR white encode above 1
        let q = select(clamp(r, vec3<f32>(0.0), vec3<f32>(1.0)), max(r, vec3<f32>(0.0)), hdr);
        e = vec3<f32>(srgb_exact(q.x), srgb_exact(q.y), srgb_exact(q.z));
    }
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
        if (pu(F_GRAIN_MODEL) == 1u) {
            e = crystal_grain(e, gx, gy);
        } else {
            let sc = pf(F_GRAIN_SC);
            let rough = pf(F_GRAIN_ROUGH);
            let seed = pu(F_GRAIN_SEED);
            var g = grain_noise(gx * sc, gy * sc, seed);
            g = g * (1.0 - rough * 0.5) + grain_noise(gx * sc * 2.3, gy * sc * 2.3, seed ^ 0x55u) * rough * 0.7;
            var lum = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
            if (hdr) {
                lum = clamp(lum, 0.0, 1.0);
            }
            let k = pf(F_GRAIN_AMT) * g * (0.35 + 2.6 * lum * (1.0 - lum));
            e = e + k;
        }
    }
    if (pu(F_LUT) != 0u) {
        let lc = clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
        let m = lut_apply(lc);
        if (hdr) {
            // the LUT sees SDR values; the part above SDR white is carried over
            e = e + (m - lc) * pf(F_LUT_AMT);
        } else {
            e = e + (m - e) * pf(F_LUT_AMT);
        }
    }
    if (warn == 1u) {
        e = vec3<f32>(PROOF_DEST_R, PROOF_DEST_G, PROOF_DEST_B);
    } else if (warn == 2u) {
        e = vec3<f32>(PROOF_DISP_R, PROOF_DISP_G, PROOF_DISP_B);
    }
    if (deep) {
        // `finish_deep`: linear, 0..peak. The sign bit a value in that range leaves free says the
        // word was written (`DEEP_WRITTEN`: `out` starts cleared, and 0 is a valid value).
        let o3 = 3u * i;
        for (var k = 0u; k < 3u; k++) {
            let v = select(0.0, min(srgb_linear(e[k]), peak), e[k] > 0.0);
            out[o3 + k] = bitcast<u32>(v) | DEEP_WRITTEN;
        }
        return;
    }
    e = vec3<f32>(out_encode(e.x), out_encode(e.y), out_encode(e.z));
    let abs_x = u32(pf(F_VIG_VIEW)) + x;
    let abs_y = u32(pf(F_VIG_VIEW + 1u)) + y;
    let dither = (BAYER_8X8[(abs_y & 7u) * 8u + (abs_x & 7u)] + 0.5) / 64.0;
    out[i] = enc8(e.x, dither) | (enc8(e.y, dither) << 8u) | (enc8(e.z, dither) << 16u) | (255u << 24u);
}
