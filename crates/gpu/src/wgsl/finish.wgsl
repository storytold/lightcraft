// The per-pixel stage: a straight port of `lightcraft_pipeline::finish` (keep in step with it).
// Bindings: img (rgb, pre-exposure), log_l, base, clar, tex, dark, masks (NMASK planes, then the
// blurred chromaticity when HAS_CHROMA), aux
// (tone LUT | sRGB LUT | curve LUTs | tone stage LUTs | mask terms), out (packed RGBA8).

// `tone::tone_eval`: the tone table at `aux` offset `o` (linear below its first entry).
fn tone_at(o: u32, y: f32) -> f32 {
    if (y <= 0.0) {
        return 0.0;
    }
    let ev = log2(y / GREY);
    let f = clamp((ev - TONE_MIN_EV) / (TONE_MAX_EV - TONE_MIN_EV), 0.0, 1.0) * f32(TONE_N - 1u);
    let i = min(u32(f), TONE_N - 2u);
    let t = f - f32(i);
    let v = aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
    if (ev < TONE_MIN_EV) {
        return v * (y / (GREY * TONE_MIN_GAIN));
    }
    return v;
}

fn tone_apply(y: f32) -> f32 {
    return tone_at(0u, y);
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

// `perceptual::rgb_to_hsv` (hue in degrees).
fn rgb_hsv(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.x, max(c.y, c.z));
    let mn = min(c.x, min(c.y, c.z));
    let d = mx - mn;
    var h = 0.0;
    if (d != 0.0) {
        if (mx == c.x) {
            h = rem_euclid((c.y - c.z) / d, 6.0);
        } else if (mx == c.y) {
            h = (c.z - c.x) / d + 2.0;
        } else {
            h = (c.x - c.y) / d + 4.0;
        }
    }
    var s = 0.0;
    if (mx > 0.0) {
        s = d / mx;
    }
    return vec3<f32>(h * 60.0, s, mx);
}

// `perceptual::hsv_to_rgb` (hue in degrees).
fn hsv_rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let c = v * s;
    let hp = rem_euclid(h, 360.0) / 60.0;
    let x = c * (1.0 - abs(hp % 2.0 - 1.0));
    var q = vec3<f32>(c, 0.0, x);
    let k = u32(hp);
    if (k == 0u) {
        q = vec3<f32>(c, x, 0.0);
    } else if (k == 1u) {
        q = vec3<f32>(x, c, 0.0);
    } else if (k == 2u) {
        q = vec3<f32>(0.0, c, x);
    } else if (k == 3u) {
        q = vec3<f32>(0.0, x, c);
    } else if (k == 4u) {
        q = vec3<f32>(x, 0.0, c);
    }
    return q + (v - c);
}

// `ColorOps::mix`: the colour mixer on linear ProPhoto `q` (HSV).
fn mix_bands(q: vec3<f32>) -> vec3<f32> {
    let p = rgb_hsv(q);
    if (p.z <= 0.0 || p.y <= 0.0) {
        return q;
    }
    var w: array<f32, 8>;
    for (var i = 0u; i < 8u; i++) {
        let a = pf(F_MIX_C + i);
        let span = rem_euclid(pf(F_MIX_C + (i + 1u) % 8u) - a, 360.0);
        let d = rem_euclid(p.x - a, 360.0);
        if (d <= span) {
            let s = 0.5 - 0.5 * cos(d / span * PI);
            w[i] += 1.0 - s;
            w[(i + 1u) % 8u] += s;
            break;
        }
    }
    var dh = 0.0;
    var ds = 0.0;
    var dd = 0.0;
    var dv = 0.0;
    for (var i = 0u; i < 8u; i++) {
        dh += w[i] * pf(F_MIX_HUE + i);
        ds += w[i] * pf(F_MIX_SAT + i);
        dd += w[i] * pf(F_MIX_DESAT + i);
        dv += w[i] * pf(F_MIX_LUM + i);
    }
    let r = hsv_rgb(p.x + dh, p.y, p.z);
    let hi = max(r.x, max(r.y, r.z));
    let lo = min(r.x, min(r.y, r.z));
    let l = (hi + lo) / 2.0;
    var f = exp(ds) * max(1.0 + dd, 0.0);
    if (lo >= 0.0 && l > lo) {
        f = min(f, l / (l - lo));
    }
    let g = 1.0 + pow(min(p.y * pow(p.z, 1.0 / 3.0) / MIX_LUM_CHROMA, 1.0), MIX_LUM_POW) * (exp2(dv) - 1.0);
    let fs = f * pow(max(g, 0.0), MIX_LUM_SPREAD);
    return l * g + (r - l) * fs;
}

// `colorops::vibrance` on linear ProPhoto `q`.
fn vibrance(q: vec3<f32>, a: f32) -> vec3<f32> {
    let p = rgb_hsv(q);
    if (p.z <= 0.0 || p.y <= 0.0) {
        return q;
    }
    let rest = max(1.0 - p.y, 0.0);
    var e = 0.0;
    var dv = 0.0;
    if (a > 0.0) {
        let d = rem_euclid(p.x - SKIN_H + 180.0, 360.0) - 180.0;
        let skin = 1.0 - VIB_P3 * exp(-((d / SKIN_W) * (d / SKIN_W)));
        e = a * VIB_P0 * pow(rest, VIB_P1) * skin;
        dv = a * VIB_P2 * skin;
    } else {
        e = a * VIB_N0 * pow(rest, VIB_N1);
        dv = a * VIB_N2 * pow(min(p.y, 1.0), VIB_N3);
    }
    return hsv_rgb(p.x, min(p.y * exp(e), max(p.y, 1.0)), p.z * exp2(dv));
}

// `colorops::saturation` on linear ProPhoto `q`.
fn saturation(q: vec3<f32>, a: f32) -> vec3<f32> {
    let y = dot(q, vec3<f32>(PP_LUMA_R, PP_LUMA_G, PP_LUMA_B));
    var f = max(1.0 + a, 0.0);
    if (a > 0.0) {
        let s = clamp(rgb_hsv(q).y, 0.0, 1.0);
        f = 1.0 + a * SAT_P0 * pow(1.0 - s, SAT_P1);
    }
    return y + (q - y) * f;
}

// `GradeK::apply` on linear ProPhoto `q`.
fn grade(q: vec3<f32>) -> vec3<f32> {
    let x = log2(max(dot(q, vec3<f32>(PP_LUMA_R, PP_LUMA_G, PP_LUMA_B)), 1e-6));
    let m = (x - pf(F_GRADE_MU + 1u)) / pf(F_GRADE_SG + 1u);
    let w = array<f32, 4>(
        1.0 / (1.0 + exp((x - pf(F_GRADE_MU)) / pf(F_GRADE_SG))),
        exp(-m * m),
        1.0 / (1.0 + exp((pf(F_GRADE_MU + 2u) - x) / pf(F_GRADE_SG + 2u))),
        1.0 / (1.0 + exp((x - pf(F_GRADE_MU + 3u)) / pf(F_GRADE_SG + 3u))),
    );
    var ln = vec3<f32>(0.0);
    for (var k = 0u; k < 4u; k++) {
        let o = F_GRADE_D + 3u * k;
        ln += w[k] * vec3<f32>(pf(o), pf(o + 1u), pf(o + 2u));
    }
    return q * exp(ln);
}

// `ColorOps::apply`.
fn color_ops(rgb0: vec3<f32>, local_sat: f32, local_hue: f32) -> vec3<f32> {
    if (pu(F_OPS_IDENTITY) != 0u && local_sat == 0.0 && local_hue == 0.0) {
        return rgb0;
    }
    var rgb = rgb0;
    let vib = pf(F_VIBRANCE);
    let sat = pf(F_SATURATION);
    if (pu(F_MIXER) != 0u || vib != 0.0 || sat != 0.0) {
        var q = mul3(mat_at(F_CURVE_M), rgb0);
        if (pu(F_MIXER) != 0u) {
            q = mix_bands(q);
        }
        if (vib != 0.0) {
            q = vibrance(q, vib);
        }
        if (sat != 0.0) {
            q = saturation(q, sat);
        }
        rgb = mul3(mat_at(F_CURVE_MI), q);
    }
    if (pu(F_NPC) > 0u || pu(F_BW) != 0u || local_sat != 0.0 || local_hue != 0.0) {
        let lab0 = oklab(rgb);
        var l = lab0.x;
        var c = sqrt(lab0.y * lab0.y + lab0.z * lab0.z);
        var h = atan2(lab0.z, lab0.y);
        for (var k = 0u; k < pu(F_NPC); k++) {
            let r = point_color(k, vec3<f32>(l, c, h));
            l = r.x;
            c = r.y;
            h = r.z;
        }
        if (local_sat != 0.0) {
            c *= max(1.0 + local_sat, 0.0);
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
        rgb = oklab_inv(vec3<f32>(l, c * cos(h), c * sin(h)));
    }
    return rgb;
}

// `tone::tone_rgb`: the table at `o` on linear ProPhoto `p`, hue-preserving (the largest and
// smallest channel go through it, the middle one keeps its relative position between them).
fn tone_rgb_at(o: u32, p: vec3<f32>) -> vec3<f32> {
    let hi = max(p.x, max(p.y, p.z));
    let lo = min(p.x, min(p.y, p.z));
    let th = tone_at(o, hi);
    let tl = tone_at(o, lo);
    var q = vec3<f32>(th);
    if (hi - lo > 1e-9) {
        q = tl + (th - tl) * (p - lo) / (hi - lo);
    }
    return q;
}

// `tone::tone_wb`: Whites / Blacks, hue-preserving moved towards a luminance gain by `ratio`.
fn tone_wb_at(o: u32, ratio: f32, p: vec3<f32>) -> vec3<f32> {
    let curve = tone_rgb_at(o, p);
    if (ratio == 0.0) {
        return curve;
    }
    let y = dot(p, vec3<f32>(PP_LUMA_R, PP_LUMA_G, PP_LUMA_B));
    var k = 0.0;
    if (y > 1e-9) {
        k = tone_at(o, y) / y;
    }
    return curve + ratio * (p * k - curve);
}

// `ToneMap::apply_rgb_hs`: in linear ProPhoto RGB, hue-preserving; Lightroom's stages when Whites /
// Blacks are set on a DNG profile curve, with Highlights / Shadows between the base operator and
// them (`F_TONE_HS_IN`) for a neighbourhood at display log luminance `ctx`.
fn tone_rgb(c: vec3<f32>, ctx: f32) -> vec3<f32> {
    let to_pp = mat_at(F_TONE_TO);
    let from_pp = mat_at(F_TONE_FROM);
    let p = mul3(to_pp, c);
    var q = vec3<f32>(0.0);
    if (pu(F_TONE_STAGED) != 0u) {
        q = tone_rgb_at(pu(F_TONE_PRE_OFF), p);
        if (pu(F_TONE_HS_IN) != 0u) {
            let shown = tone_rgb_at(pu(F_TONE_PROF_OFF), q);
            let yd = dot(shown, vec3<f32>(PP_LUMA_R, PP_LUMA_G, PP_LUMA_B));
            if (yd > 1e-9) {
                let k = lr_hs_gain(log2(max(yd, 1e-6)), ctx);
                if (k != 0.0) {
                    let at = tone_at(pu(F_TONE_PROFINV_OFF), yd);
                    if (at > 1e-12) {
                        q = q * (tone_at(pu(F_TONE_PROFINV_OFF), yd * exp2(k)) / at);
                    }
                }
            }
        }
        if (pu(F_TONE_WH) != 0u) {
            q = tone_wb_at(pu(F_TONE_WH_OFF), pf(F_TONE_KW), q);
        }
        if (pu(F_TONE_BK) != 0u) {
            q = tone_wb_at(pu(F_TONE_BK_OFF), pf(F_TONE_KB), q);
        }
        q = tone_rgb_at(pu(F_TONE_POST_OFF), q);
    } else {
        q = tone_rgb_at(0u, p);
    }
    return mul3(from_pp, q);
}

// `colorops::calibrate`: primaries matrix, then the (subtractive) shadows tint.
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
        let y = lum2020(c);
        let w = 1.0 - sstep(SHADOW_TINT_LO, SHADOW_TINT_HI, log2(max(y, 1e-7) / 0.18));
        let k = max(1.0 - SHADOW_TINT_K * abs(st) * w, 0.0);
        if (st > 0.0) {
            c.y *= k;
        } else {
            c.x *= k;
            c.z *= k;
        }
    }
    return c;
}

// `tone::lr_knot` on the table at field offset `off`.
fn lr_knot(off: u32, l: f32) -> f32 {
    let f = clamp((l - LR_K0) / LR_KSTEP, 0.0, f32(LR_KN - 1u));
    let i = min(u32(f), LR_KN - 2u);
    let t = f - f32(i);
    return pf(off + i) + (pf(off + i + 1u) - pf(off + i)) * t;
}

// `tone::LrHs::gain`: log2 gain of a pixel at display log luminance `l` in a neighbourhood at `ctx`.
fn lr_hs_gain(l: f32, ctx: f32) -> f32 {
    var o = l;
    let kh = pf(F_LR_HL);
    if (kh != 0.0) {
        o += kh * lr_knot(F_LR_HL_TAB, l + pf(F_LR_HL_ALPHA) * (ctx - l));
    }
    let ks = pf(F_LR_SH);
    if (ks != 0.0) {
        o += ks * lr_knot(F_LR_SH_TAB, l + pf(F_LR_SH_ALPHA) * (ctx - l));
    }
    return min(o, max(l, 0.0)) - l;
}

// `finish::refine_saturation` (luma weights of the curve space).
fn refine_saturation(before: vec3<f32>, after: vec3<f32>, refine: f32) -> vec3<f32> {
    let lw = vec3<f32>(pf(F_CURVE_Y), pf(F_CURVE_Y + 1u), pf(F_CURVE_Y + 2u));
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

// sRGB-encoded (clamped to 0..1) → linear.
fn decode_srgb(v: f32) -> f32 {
    let e = clamp(v, 0.0, 1.0);
    if (e <= 0.04045) {
        return e / 12.92;
    }
    return pow((e + 0.055) / 1.055, 2.4);
}

fn mat_at(o: u32) -> array<vec3<f32>, 3> {
    return array<vec3<f32>, 3>(
        vec3<f32>(pf(o), pf(o + 1u), pf(o + 2u)),
        vec3<f32>(pf(o + 3u), pf(o + 4u), pf(o + 5u)),
        vec3<f32>(pf(o + 6u), pf(o + 7u), pf(o + 8u)),
    );
}

// `Vig::falloff`: 0 inside the post-crop vignette, rising to 1 towards the frame's corners.
fn vig_falloff(x: u32, y: u32, w: u32, h: u32) -> f32 {
    let fw = f32(w);
    let fh = f32(h);
    let aspect = fw / fh;
    let mixa = pf(F_VIG_ASPECT_MIX);
    let power = pf(F_VIG_POWER);
    let u = (f32(x) + 0.5) / fw * 2.0 - 1.0;
    let vv = (f32(y) + 0.5) / fh * 2.0 - 1.0;
    let sx = 1.0 + (aspect - 1.0) * mixa;
    let sy = 1.0 + (1.0 / aspect - 1.0) * mixa;
    let ax = abs(u * max(sx, 1.0) / max(sx, sy));
    let ay = abs(vv * max(sy, 1.0) / max(sx, sy));
    let dist = pow(pow(ax, power) + pow(ay, power), 1.0 / power);
    let start = pf(F_VIG_START);
    return sstep(start, start + pf(F_VIG_WIDTH), dist);
}

// `finish::apply_curves`: the tone curves in the fixed curve space; parametric ∘ master
// hue-preserving (largest / smallest channel through it, the middle one in between), then the
// red / green / blue curves per channel.
fn apply_curves(d: vec3<f32>) -> vec3<f32> {
    let q = mul3(mat_at(F_CURVE_M), d);
    let qc = clamp(q, vec3<f32>(0.0), vec3<f32>(1.0));
    let e0 = vec3<f32>(encode_srgb(qc.x), encode_srgb(qc.y), encode_srgb(qc.z));
    let mx = max(qc.x, max(qc.y, qc.z));
    let mn = min(qc.x, min(qc.y, qc.z));
    let hi = decode_srgb(curve(0u, encode_srgb(mx)));
    let lo = decode_srgb(curve(0u, encode_srgb(mn)));
    var b = vec3<f32>(hi);
    if (mx - mn > 1e-9) {
        b = lo + (hi - lo) * (qc - mn) / (mx - mn);
    }
    let e = vec3<f32>(curve(1u, encode_srgb(b.x)), curve(2u, encode_srgb(b.y)), curve(3u, encode_srgb(b.z)));
    var lin = vec3<f32>(decode_srgb(e.x), decode_srgb(e.y), decode_srgb(e.z));
    let rs = pf(F_REFINE_SAT);
    if (rs < 1.0) {
        let lw = vec3<f32>(pf(F_CURVE_Y), pf(F_CURVE_Y + 1u), pf(F_CURVE_Y + 2u));
        let er = refine_saturation(e0, e, rs);
        let r = vec3<f32>(decode_srgb(er.x), decode_srgb(er.y), decode_srgb(er.z));
        let y0 = dot(lin, lw);
        let y1 = dot(r, lw);
        if (y1 > 1e-6) {
            lin = r * (y0 / y1);
        } else {
            lin = r;
        }
    }
    let q1 = lin + (q - qc);
    return mul3(mat_at(F_CURVE_MI), q1);
}

// `finish::interp` over the sharpening amount knots, of the table at field offset `off`.
fn sharpen_interp(off: u32, a: f32) -> f32 {
    var i = 0u;
    for (var j = 1u; j < 7u; j++) {
        if (pf(F_SH_KNOTS + j) <= a) {
            i = j;
        }
    }
    let x0 = pf(F_SH_KNOTS + i);
    let x1 = pf(F_SH_KNOTS + i + 1u);
    var t = 0.0;
    if (x1 > x0) {
        t = clamp((a - x0) / (x1 - x0), 0.0, 1.0);
    }
    return pf(off + i) + (pf(off + i + 1u) - pf(off + i)) * t;
}

// `finish::Sharpen::gain`.
fn sharpen_gain(y: f32, blur: f32, local: f32, grad: f32) -> f32 {
    let a = pf(F_SH_AMT) + local * pf(F_SH_LOCAL);
    if (a == 0.0 || !(y > 0.0) || !(blur > 0.0)) {
        return 1.0;
    }
    let at = min(abs(a), 150.0);
    let k = sharpen_interp(F_SH_K, at) * sign(a);
    let lim = max(sharpen_interp(F_SH_L, at), 1e-3);
    // the argument clamped: some drivers' tanh overflows to NaN for large inputs
    var m = lim * tanh(clamp(k * (y / blur - 1.0) / lim, -10.0, 10.0));
    let mask = pf(F_SH_MASK);
    if (mask > 0.0) {
        let e0 = SHARPEN_MASK_AT * mask;
        let width = SHARPEN_MASK_WIDTH * min(mask * 4.0, 1.0);
        m *= sstep(e0, e0 + width, grad * max(pf(F_SH_PXSRC), 1e-6));
    }
    return exp2(m);
}

// log2 of the sharpening blur at (x, y), clamped to the image.
fn sharpen_log_blur(x: i32, y: i32, w: u32, h: u32) -> f32 {
    let cx = u32(clamp(x, 0, i32(w) - 1));
    let cy = u32(clamp(y, 0, i32(h) - 1));
    return log2(max(tex[pu(F_SHARP_OFF) + cy * w + cx], 1e-9));
}

fn ghash(i: i32, j: i32, seed: u32) -> f32 {
    var v = (bitcast<u32>(i) * GRAIN_H0) ^ (bitcast<u32>(j) * GRAIN_H1) ^ (seed * GRAIN_H2);
    v ^= v >> 13u;
    v = v * GRAIN_H3;
    v ^= v >> 15u;
    return f32(v & 0xffffu) / 32768.0 - 1.0;
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
    if (tx != 0.0 && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        let tame = 1.0 - 0.6 * sstep(0.4, 1.6, abs(det));
        delta += tx * 1.1 * clamp(det, -1.0, 1.0) * tame;
    }
    // local Noise: smooth (or, negative, boost) small-amplitude detail, keep edges
    if (l_noise != 0.0 && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        delta -= clamp(l_noise, -1.0, 1.0) * 0.9 * det * (1.0 - sstep(0.1, 0.5, abs(det)));
    }
    if (delta != 0.0) {
        c = c * exp2(delta);
    }

    // --- post-crop vignette, Highlight / Colour Priority: an exposure change before the tone map
    if (pu(F_VIG) != 0u && pu(F_VIG_STYLE) != 2u) {
        let t = vig_falloff(x, y, w, h);
        if (t > 0.0) {
            var e = pf(F_VIG_STRENGTH) * t;
            if (e < 0.0 && pu(F_VIG_STYLE) == 1u && pf(F_VIG_HL) > 0.0) {
                e *= 1.0 - pf(F_VIG_HL) * sstep(0.4, 1.0, clamp(tone_apply(lum2020(c)), 0.0, 1.0));
            }
            c = c * exp(e);
        }
    }

    // --- calibration (scene linear, before the tone map)
    if (pu(F_CALIB) != 0u || pf(F_SHADOW_TINT) != 0.0) {
        c = calibrate(c);
    }

    // --- tone map: on luminance with highlight desaturation, or (a DNG profile tone curve) per
    // channel, hue-preserving. Highlights / Shadows as Lightroom applies them on a DNG profile tone
    // curve, before Whites / Blacks (inside the map when those are set) and Contrast
    var ctx = 0.0;
    if (pu(F_LR_HS) != 0u) {
        var cx = 0.0;
        if (pu(F_TONE_HS_IN) != 0u) {
            cx = tone_at(pu(F_TONE_CTX_OFF), GREY * exp2(base));
        } else {
            cx = tone_apply(GREY * exp2(base));
        }
        ctx = log2(max(cx, 1e-6));
    }
    var d = vec3<f32>(0.0);
    if (pu(F_TONE_RGB) != 0u) {
        d = tone_rgb(c, ctx);
    } else {
        let yl = lum2020(c);
        let o = tone_apply(yl);
        if (yl > 1e-9) {
            d = c * o / yl;
        }
        let mx = max(d.x, max(d.y, d.z));
        if (mx > 1.0) {
            let t = clamp((mx - 1.0) / max(mx - o, 1e-6), 0.0, 1.0);
            d = d + (o - d) * t;
        }
    }
    if (pu(F_LR_HS) != 0u) {
        if (pu(F_TONE_HS_IN) == 0u) {
            let k = lr_hs_gain(log2(max(lum2020(d), 1e-6)), ctx);
            if (k != 0.0) {
                d = d * exp2(k);
            }
        }
        if (pu(F_TONE_CON) != 0u) {
            d = mul3(mat_at(F_TONE_FROM), tone_rgb_at(pu(F_TONE_CON_OFF), mul3(mat_at(F_TONE_TO), d)));
        }
    }

    // --- colour
    d = color_ops(d, lt[11], lt[12]);
    if (tint_on) {
        let lab = oklab(d);
        d = oklab_inv(vec3<f32>(lab.x, lab.y + tint_dir.x * 0.08 * tint_amt, lab.z + tint_dir.y * 0.08 * tint_amt));
    }

    // --- Paint Overlay vignette (display linear, post-crop): towards black or white
    if (pu(F_VIG) != 0u && pu(F_VIG_STYLE) == 2u) {
        let t = vig_falloff(x, y, w, h);
        if (t > 0.0) {
            let amount = pf(F_VIG_AMOUNT);
            if (amount < 0.0) {
                d = d * (1.0 + amount * t);
            } else {
                d = d + (1.0 - d) * amount * t * 0.85;
            }
        }
    }

    // --- tone curves, in the fixed curve space, then colour grading
    if (pu(F_CURVES) != 0u) {
        d = apply_curves(d);
    }
    if (pu(F_GRADING) != 0u) {
        d = mul3(mat_at(F_CURVE_MI), grade(mul3(mat_at(F_CURVE_M), d)));
    }

    // --- sharpening on the finished image: the first pass writes the luminance, the second
    // (after the host blurred it) applies the gain
    let sh_pass = pu(F_SH_PASS);
    if (sh_pass == 1u) {
        out[i] = bitcast<u32>(max(lum2020(d), 0.0));
        return;
    }
    if (sh_pass == 2u) {
        let b = tex[pu(F_SHARP_OFF) + i];
        var grad = 0.0;
        if (pf(F_SH_MASK) > 0.0) {
            let xi = i32(x);
            let yi = i32(y);
            let gx = sharpen_log_blur(xi + 1, yi, w, h) - sharpen_log_blur(xi - 1, yi, w, h);
            let gy = sharpen_log_blur(xi, yi + 1, w, h) - sharpen_log_blur(xi, yi - 1, w, h);
            grad = 0.5 * sqrt(gx * gx + gy * gy);
        }
        let g = sharpen_gain(max(lum2020(d), 0.0), b, lt[13], grad);
        if (g != 1.0) {
            d = d * g;
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

    // --- encode, grain
    var e = vec3<f32>(encode_srgb(r.x), encode_srgb(r.y), encode_srgb(r.z));
    if (pu(F_GRAIN) != 0u) {
        let px = f32(x) + 0.5;
        let py = f32(y) + 0.5;
        let gx = pf(F_GRAIN_AFF) * px + pf(F_GRAIN_AFF + 2u) * py + pf(F_GRAIN_AFF + 4u);
        let gy = pf(F_GRAIN_AFF + 1u) * px + pf(F_GRAIN_AFF + 3u) * py + pf(F_GRAIN_AFF + 5u);
        let sc = pf(F_GRAIN_SC);
        let rough = pf(F_GRAIN_ROUGH);
        let seed = pu(F_GRAIN_SEED);
        var g = grain_noise(gx * sc, gy * sc, seed);
        g = g * (1.0 - rough * 0.5) + grain_noise(gx * sc * 2.3, gy * sc * 2.3, seed ^ 0x55u) * rough * 0.7;
        let lum = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
        let k = pf(F_GRAIN_AMT) * g * (0.35 + 2.6 * lum * (1.0 - lum));
        e = e + k;
    }
    e = vec3<f32>(out_encode(e.x), out_encode(e.y), out_encode(e.z));
    out[i] = enc8(e.x) | (enc8(e.y) << 8u) | (enc8(e.z) << 16u) | (255u << 24u);
}
