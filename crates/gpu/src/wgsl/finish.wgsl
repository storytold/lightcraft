// The per-pixel stage: a straight port of `lcc_pipeline::finish` (keep in step with it).
// Bindings: img (rgb, pre-exposure), log_l, base, clar, tex, dark, masks (NMASK planes, then the
// blurred chromaticity when HAS_CHROMA), aux
// (tone LUT | chroma curve | sRGB LUT | curve LUTs | mask terms), out (packed RGBA8; for a deep
// render, `DEEP`, also linear float R, G, B in the output primaries from word `DEEP_OFF`, and the
// packed pixel is their 8-bit reduction, `DeepImage::to_rgba8`). `PEAK` is the HDR render's peak
// (SDR white = 1; 1 for SDR renders).

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

fn rgb_contrast_apply(v: f32, base: u32, zero: f32, peak: f32) -> f32 {
    if (v <= 0.0) {
        return zero;
    }
    if (v >= peak) {
        return aux[base + RGB_CONTRAST_N - 1u] + (v - peak);
    }
    let u = sqrt(v / peak);
    let f = u * f32(RGB_CONTRAST_N - 1u);
    let i = min(u32(f), RGB_CONTRAST_N - 2u);
    let t = f - f32(i);
    let a = aux[base + i];
    let b = aux[base + i + 1u];
    return a + (b - a) * t;
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

// The curve table at `o` in `aux` (`Lut1::eval`).
fn lut_at(o: u32, x: f32) -> f32 {
    let f = clamp(x, 0.0, 1.0) * f32(CURVE_N - 1u);
    let i = min(u32(f), CURVE_N - 2u);
    let t = f - f32(i);
    return aux[o + i] + (aux[o + i + 1u] - aux[o + i]) * t;
}

// `finish::eval_curves` on one channel: for HDR renders (`hdr`) a value above SDR white continues
// with slope 1 from the curve's end point.
fn lut_hdr(o: u32, x: f32, hdr: bool) -> f32 {
    if (hdr && x > 1.0) {
        return lut_at(o, 1.0) + (x - 1.0);
    }
    return lut_at(o, x);
}

fn curve(ch: u32, x: f32, hdr: bool) -> f32 {
    return lut_hdr(pu(F_CURVE_OFF) + ch * CURVE_N, x, hdr);
}

// `levels::StageK::eval` on one value: the stage record at word `o` of the parameters (active, black, 1/range,
// out black, span, slope at 0, slope at 1, has a midtone table) and the table's place `off` in `aux`. The stretch is not
// limited: the midtone curve (its samples, then its end slopes beyond 0 and 1) runs on, and the result is limited below
// at 0, and above at 1 unless the render is HDR.
const MIDTONE_N: u32 = 257u;
fn levels_stage(o: u32, off: u32, x: f32, hdr: bool) -> f32 {
    if (pf(o) == 0.0) {
        return x;
    }
    let t = (x - pf(o + 1u)) * pf(o + 2u);
    var g = t;
    if (pf(o + 7u) != 0.0) {
        if (t <= 0.0) {
            g = pf(o + 5u) * t;
        } else if (t >= 1.0) {
            g = 1.0 + pf(o + 6u) * (t - 1.0);
        } else {
            let f = t * f32(MIDTONE_N - 1u);
            let i = min(u32(f), MIDTONE_N - 2u);
            g = aux[off + i] + (aux[off + i + 1u] - aux[off + i]) * (f - f32(i));
        }
    }
    var y = max(pf(o + 3u) + pf(o + 4u) * g, 0.0);
    if (!hdr) {
        y = min(y, 1.0);
    }
    return y;
}

// `brightness::curve`: v·E / (1 + v·(E − 1)) on 0..1. Above 1 an SDR render holds 1; an HDR render (headroom `h`, the encoded
// peak) applies the same curve rescaled onto 1..h and passes values above `h` through. Below 0 the result is 0.
// `e` is E = e^phi from the parameter block.
fn brightness_curve(v0: f32, e: f32, hdr: bool, h: f32) -> f32 {
    let v = min(max(v0, 0.0), 10000.0);
    if (v < 1.0) {
        return v * e / (1.0 + v * (e - 1.0));
    }
    if (!hdr) {
        return 1.0;
    }
    if (v < h) {
        let r = h - 1.0;
        let x = (v - 1.0) / r;
        return 1.0 + r * (x * e / (1.0 + x * (e - 1.0)));
    }
    return v;
}

// One channel of `BrightnessK::apply`: the brightest takes `vp`, the darkest `mp`, the others sit between by hue.
fn brightness_channel(v: f32, mx: f32, mn: f32, range: f32, vp: f32, mp: f32) -> f32 {
    if (v >= mx) {
        return vp;
    }
    if (v <= mn) {
        return mp;
    }
    return mp + (v - mn) / range * (vp - mp);
}

// `BrightnessK::apply_with`: the brightest channel gets the curve, the hue is kept, the saturation is a blend of the
// original and the per-channel one (weight `F_BRIGHTNESS_W`).
fn brightness_core(c0: vec3<f32>, hdr: bool, h: f32) -> vec3<f32> {
    let e = pf(F_BRIGHTNESS_E);
    let w = pf(F_BRIGHTNESS_W);
    let c = min(max(c0, vec3<f32>(0.0)), vec3<f32>(10000.0));
    let mx = max(c.x, max(c.y, c.z));
    let mn = min(c.x, min(c.y, c.z));
    if (mx <= 0.0) {
        return vec3<f32>(0.0);
    }
    let vp = brightness_curve(mx, e, hdr, h);
    let range = mx - mn;
    if (range <= 0.0) {
        return vec3<f32>(vp);
    }
    let mp = (1.0 - w) * vp * (mn / mx) + w * brightness_curve(mn, e, hdr, h);
    return vec3<f32>(
        brightness_channel(c.x, mx, mn, range, vp, mp),
        brightness_channel(c.y, mx, mn, range, vp, mp),
        brightness_channel(c.z, mx, mn, range, vp, mp));
}

// `BrightnessK::apply`
fn brightness_apply(c0: vec3<f32>, hdr: bool) -> vec3<f32> {
    return brightness_core(c0, hdr, max(pf(F_BRIGHTNESS_H), 1.0));
}

// The exact sRGB curve (`lcc_color::transfer`), unbounded above 1 as HDR renders encode.
fn lin_to_srgb(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn srgb_to_lin(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
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
    var w: array<f32, POINT_WORDS>;
    for (var j = 0u; j < POINT_WORDS; j++) {
        w[j] = pf(o + j);
    }
    return point_apply(w, lch, 1.0);
}

// `PointK::apply_scaled`: the sample's words `pw` (`PointK::words`) at `strength`.
fn point_apply(pw: array<f32, POINT_WORDS>, lch: vec3<f32>, strength: f32) -> vec3<f32> {
    let sl = pw[0];
    let sc = pw[1];
    let sh = pw[2];
    let var_k = pw[6];
    let wh_ = pw[7];
    let wc_ = pw[8];
    let wl_ = pw[9];
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
    let w = wh * wc * wl * strength;
    if (w <= 0.0) {
        return lch;
    }
    h += w * (var_k * wrap_angle(h - sh) + pw[3]);
    c += w * var_k * (c - sc);
    c = max(c * (1.0 + w * pw[4]), 0.0);
    l += w * (var_k * (l - sl) + pw[5]);
    return vec3<f32>(l, c, h);
}

// `colorops::mixer_apply` for the mixer whose words (`MixerK::words`) start at `aux[o]`, at `strength`.
fn local_mixer(o: u32, lch: vec3<f32>, strength: f32) -> vec3<f32> {
    let w = band_weights(lch.z);
    var dh = 0.0;
    var ds = 0.0;
    var dl = 0.0;
    for (var i = 0u; i < 8u; i++) {
        dh += w[i] * aux[o + i];
        ds += w[i] * aux[o + 8u + i];
        dl += w[i] * aux[o + 16u + i];
    }
    let chroma_w = min(lch.y / 0.12, 1.0);
    let h = lch.z + dh * strength * chroma_w;
    let c = lch.y * max(1.0 + ds * strength, 0.0);
    let l = lch.x + dl * strength * chroma_w * sqrt(max(lch.x, 0.05));
    return vec3<f32>(l, c, h);
}

// `finish::local_point_colors`: each mask's Color Mixer, then its Point Color samples at the mask's opacity (at most 1).
// `i`, `n`: the pixel's index in the mask planes and their stride (`main`'s `mi`, `mn`).
fn local_point_colors(d: vec3<f32>, i: u32, n: u32) -> vec3<f32> {
    var lch = vec3<f32>(0.0);
    var any = false;
    for (var m = 0u; m < pu(F_NMASK); m++) {
        let rec = pu(F_LPOINT_OFF) + m * LOCAL_POINT_WORDS;
        let np = min(u32(aux[rec]), 8u);
        let has_mixer = aux[rec + 1u] != 0.0;
        if (np == 0u && !has_mixer) {
            continue;
        }
        let a = min(masks[m * n + i], 1.0);
        if (a <= 0.0) {
            continue;
        }
        if (!any) {
            let lab = oklab(d);
            let c0 = sqrt(lab.y * lab.y + lab.z * lab.z);
            // (atan2(0, 0) is undefined in WGSL: a neutral pixel's hue is 0, as on the CPU)
            lch = vec3<f32>(lab.x, c0, select(atan2(lab.z, lab.y), 0.0, c0 == 0.0));
            any = true;
        }
        if (has_mixer) {
            lch = local_mixer(rec + 2u, lch, a);
        }
        for (var k = 0u; k < np; k++) {
            var w: array<f32, POINT_WORDS>;
            for (var j = 0u; j < POINT_WORDS; j++) {
                w[j] = aux[rec + LOCAL_POINT_SAMPLES + k * POINT_WORDS + j];
            }
            lch = point_apply(w, lch, a);
        }
    }
    if (!any) {
        return d;
    }
    return oklab_inv(vec3<f32>(lch.x, lch.y * cos(lch.z), lch.y * sin(lch.z)));
}

// `PrimariesK::apply` (the colourist wheels). The Color wheels are ASC CDL's SOP per channel —
// lift's affine `x·(1−l)+l` pivots at white, gain's multiply at black, gamma a power, offset adds
// last — then the Log wheels refine over their narrow zones between PRIM_LOW and PRIM_HIGH.
fn primaries(rgb: vec3<f32>) -> vec3<f32> {
    if (pu(F_PRIMARIES) == 0u) {
        return rgb;
    }
    let t = clamp(lum2020(rgb), 0.0, 1.0);
    let lm = clamp(pf(F_PRIM_LOW), 0.02, 0.9);
    let hm = clamp(pf(F_PRIM_HIGH), lm + 0.05, 0.98);
    let lws = 1.0 - sstep(lm - 0.10, lm + 0.10, t);
    let lwh = sstep(hm - 0.10, hm + 0.10, t);
    let lwm = max(1.0 - lws - lwh, 0.0);
    var o = vec3<f32>(0.0);
    for (var k = 0u; k < 3u; k++) {
        let l = pf(F_PRIM_LIFT + k) + pf(F_PRIM_LIFT + 3u);
        let g = pf(F_PRIM_GAIN + k) + pf(F_PRIM_GAIN + 3u);
        let p = pf(F_PRIM_GAMMA + k) + pf(F_PRIM_GAMMA + 3u);
        var x = rgb[k] * (1.0 - l) + l;
        x = max(x * (1.0 + g), 0.0);
        let e = clamp(1.0 / (1.0 + p), 0.25, 4.0);
        x = pow(max(x, 0.0), e);
        x += pf(F_PRIM_OFFSET + k) + pf(F_PRIM_OFFSET + 3u);
        x += (pf(F_PRIM_SHADOW + k) + pf(F_PRIM_SHADOW + 3u)) * lws;
        x += (pf(F_PRIM_MIDTONE + k) + pf(F_PRIM_MIDTONE + 3u)) * lwm;
        x *= 1.0 + (pf(F_PRIM_HIGHLIGHT + k) + pf(F_PRIM_HIGHLIGHT + 3u)) * lwh;
        x += pf(F_PRIM_LOG_OFFSET + k) + pf(F_PRIM_LOG_OFFSET + 3u);
        o[k] = max(x, 0.0);
    }
    return o;
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
    // `HslK` (the HSL curves): the hue picks the place on each LUT — hue[128], sat[128], lum[128]
    // in that order (HSL_WORDS in `lcc_pipeline::colorops`) — and the mixer's own scales apply the
    // deltas, fading hue and luminance towards neutrals as the mixer does
    if (pu(F_HSL) != 0u) {
        let n = 128u;
        let x = rem_euclid(h / TAU, 1.0);
        let f = clamp(x, 0.0, 0.999999) * f32(n - 1u);
        let i0 = u32(f);
        let i1 = min(i0 + 1u, n - 1u);
        let t = f - f32(i0);
        let dh = mix(pf(F_HSL_LUT + i0), pf(F_HSL_LUT + i1), t);
        let ds = mix(pf(F_HSL_LUT + n + i0), pf(F_HSL_LUT + n + i1), t);
        let dl = mix(pf(F_HSL_LUT + 2u * n + i0), pf(F_HSL_LUT + 2u * n + i1), t);
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

fn ghash(i: i32, j: i32, seed: u32) -> f32 {
    var v = (bitcast<u32>(i) * GRAIN_VH0) ^ (bitcast<u32>(j) * GRAIN_VH1) ^ (seed * GRAIN_VH2);
    v ^= v >> 13u;
    v = v * GRAIN_VH3;
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

// --- film grain: a port of `lcc_pipeline::grain` (keep in step with it)
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

// `coverage_std_at`: the standard deviation of the field (coverage less halo) at coverage `p`
fn grain_std_at(p: f32, area: f32, kappa: f32, k_hh: f32, k_ah: f32) -> f32 {
    let c = pf(F_GRAIN_CELL);
    let q = 1.0 - p;
    let t = -log(q) / area * c * c;
    let fw = min(t, 1.0);
    let mu = -log(q) * (1.0 - fw / (6.0 * max(t, 1e-12)));
    return sqrt(max(q * q * (exp(kappa * mu) - 1.0 + mu * (k_hh - 2.0 * k_ah)), 0.0));
}

fn grain_per_cell(p: f32) -> f32 {
    let c = pf(F_GRAIN_CELL);
    return -log(1.0 - p) / pf(F_GRAIN_AREA) * c * c;
}

fn grain_std(p: f32) -> f32 {
    return grain_std_at(p, pf(F_GRAIN_AREA), pf(F_GRAIN_KAPPA), pf(F_GRAIN_K_HH), pf(F_GRAIN_K_AH));
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

// radius and density of grain `hj`
fn grain_sub_size(hj: u32) -> vec2<f32> {
    let rad = gpcg(hj ^ GRAIN_H3);
    let z = 2.0 * (gunit8(rad) + gunit8(rad >> 8u) + gunit8(rad >> 16u)) - 3.0;
    return vec2<f32>(exp(pf(F_GRAIN_SPREAD) * z), 0.25 + 0.75 * gunit8(rad >> 24u));
}

// the halo of a clump of weight `w` at squared distance `d2` from its centre (`clump_halo`)
fn grain_clump_halo(w: f32, d2: f32) -> f32 {
    let u = d2 * pf(F_GRAIN_HALO_OUT);
    if (u >= 1.0 || w == 0.0) {
        return 0.0;
    }
    let v = d2 * pf(F_GRAIN_HALO_IN);
    var inner = 0.0;
    if (v < 1.0) {
        inner = (1.0 - v) * (1.0 - v);
    }
    return pf(F_GRAIN_HALO) * w * ((1.0 - u) * (1.0 - u) - inner);
}

// coverage of grain `hj` at offset (dx, dy) from its centre: radius, density, elongation,
// orientation, lobed outline, soft edge (0 beyond its reach)
fn grain_sub_cover(hj: u32, dx: f32, dy: f32, d2: f32) -> f32 {
    let s = pf(F_GRAIN_S);
    let lump = pf(F_GRAIN_LUMP);
    let ro = grain_sub_size(hj);
    let r0 = ro.x;
    let o = ro.y;
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
            let p2 = grain_phase((shp >> 20u) & 15u);
            let p3 = grain_phase((shp >> 24u) & 15u);
            let c2 = c * c - sn * sn;
            let s2 = 2.0 * c * sn;
            let c3 = c * (4.0 * c * c - 3.0);
            let s3 = sn * (3.0 - 4.0 * sn * sn);
            r = r * (1.0 + lump * (0.6 * m * (p2.x * c2 + p2.y * s2) + 0.4 * m * (p3.x * c3 + p3.y * s3)));
        }
    }
    return o * grain_profile(d, r, s, pf(F_GRAIN_INV_S));
}

// a disc of radius `r` softened by the edge width `s`, at distance `d` from its centre
fn grain_profile(d: f32, r: f32, s: f32, inv_s: f32) -> f32 {
    let rho2 = r * r + s * s;
    let tau = (d - sqrt(rho2)) * inv_s;
    if (tau >= 1.0) {
        return 0.0;
    }
    var prof = 1.0;
    if (tau > -1.0) {
        prof = (1.0 - tau) * (1.0 - tau) * (2.0 + tau) * 0.25;
    }
    return r * r / (rho2 + s * s * 0.2) * prof;
}

// coverage (the union of its grains) and halo (a ring around its centre) of clump `h` at offset
// (dx, dy) from its centre
fn grain_clump_cover(h: u32, dx: f32, dy: f32) -> vec2<f32> {
    let sr = pf(F_GRAIN_SUB_REACH);
    let count = grain_sub_count(h);
    var t = 1.0;
    var w = 0.0;
    let has_halo = pf(F_GRAIN_HALO) > 0.0;
    for (var j = 0u; j < count; j++) {
        let hj = gsub_hash(h, j);
        if (has_halo) {
            let ro = grain_sub_size(hj);
            w += ro.y * ro.x * ro.x;
        }
        let off = grain_sub_offset(hj);
        let ex = dx - off.x;
        let ey = dy - off.y;
        let d2 = ex * ex + ey * ey;
        if (d2 >= sr * sr) {
            continue;
        }
        t *= 1.0 - grain_sub_cover(hj, ex, ey, d2);
    }
    return vec2<f32>(1.0 - t, grain_clump_halo(w, dx * dx + dy * dy));
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
    var hs = 0.0;
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
                let ah = grain_clump_cover(h, ex, ey);
                if (ah.x == 0.0 && ah.y == 0.0) {
                    continue;
                }
                var f = 1.0;
                if (prod < e_full) {
                    f = clamp((end + log(prod)) / fw, 0.0, 1.0);
                }
                keep *= 1.0 - f * ah.x;
                hs += f * ah.y;
            }
        }
    }
    // less the halo's deviation from its mean, on the light the grains let through
    var halo_mean = 0.0;
    if (pf(F_GRAIN_HALO) > 0.0) {
        halo_mean = -log(1.0 - p) * pf(F_GRAIN_HALO_MEAN);
    }
    return 1.0 - keep - (1.0 - p) * (hs - halo_mean);
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
        let ix = i32(floor(gx * ppl));
        let iy = i32(floor(gy * ppl));
        var n = grain_gauss(ix, iy, seed);
        let e = pf(F_GRAIN_NOISE_E);
        if (e != 0.0) {
            // correlated with its four neighbours (`GrainParams::noise`)
            let around = grain_gauss(ix - 1, iy, seed) + grain_gauss(ix + 1, iy, seed) + grain_gauss(ix, iy - 1, seed) + grain_gauss(ix, iy + 1, seed);
            n = (n - e * around) * pf(F_GRAIN_NOISE_N);
        }
        d -= wn * grain_std(p) * n;
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
    // equally strong at every tone (at the reference resolution), tilted by the response
    let tilt = pf(F_GRAIN_TILT);
    var tw = 1.0;
    if (tilt != 0.0) {
        tw = exp(tilt * (l - 0.5));
    }
    let sd_ref = grain_std_at(p, pf(F_GRAIN_AREA_REF), pf(F_GRAIN_KAPPA_REF), pf(F_GRAIN_K_HH_REF), pf(F_GRAIN_K_AH_REF));
    let amp = pf(F_GRAIN_AMP) * tw / max(sd_ref, 1e-4);
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

// `finish::enc`: 8 bits, adding `d` (0 to 1; a half rounds) before truncating; white stays white.
fn enc8(v: f32, d: f32) -> u32 {
    let y = clamp(v, 0.0, 1.0) * 255.0;
    if (y >= 254.9) {
        return 255u;
    }
    return u32(clamp(y + d, 0.0, 255.0));
}

// `finish::bayer_dither_offset` at output pixel (`x`, `y`) of the whole output: the 8×8 Bayer
// threshold is the bits of `x ^ y` and `y` interleaved, lowest first, read in reverse.
fn bayer_dither(x: u32, y: u32) -> f32 {
    let a = x ^ y;
    let t = ((a & 1u) << 5u) | ((y & 1u) << 4u) | ((a & 2u) << 2u) | ((y & 2u) << 1u) | ((a & 4u) >> 1u) | ((y & 4u) >> 2u);
    return (f32(t) + 0.5) / 64.0;
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

// The 3×3 matrix stored row by row from field `o`.
fn mat_at(o: u32) -> array<vec3<f32>, 3> {
    return array<vec3<f32>, 3>(
        vec3<f32>(pf(o), pf(o + 1u), pf(o + 2u)),
        vec3<f32>(pf(o + 3u), pf(o + 4u), pf(o + 5u)),
        vec3<f32>(pf(o + 6u), pf(o + 7u), pf(o + 8u)),
    );
}

// `finish::gamut_map` (SDR): the chroma scale that brings `r` into 0..1 towards its luminance (1: in gamut) ...
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

// `finish::out_of_gamut`: `r` needed desaturating by `t` to fit and isn't just a neutral beyond white or below black.
fn out_of_gamut(r: vec3<f32>, t: f32) -> bool {
    let lo = min(r.x, min(r.y, r.z));
    let hi = max(r.x, max(r.y, r.z));
    return t < 0.995 && (hi - lo) > 0.02 * max(abs(hi), 1e-3) && hi - lo > 1e-3 && (lo < -1e-3 || (hi > 1.0 && lo < 1.0));
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
    // the mask planes' stride and this band's first pixel in `masks`: the whole image's planes
    // (MASK_N = 0), or a band's rows copied out of planes too big for one buffer (`render`)
    let mn = select(n, pu(F_MASK_N), pu(F_MASK_N) != 0u);
    let mi = i - pu(F_MASK_I0);
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
        let a = masks[m * mn + mi];
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
        let o = nm * mn + 3u * mi;
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
        if (dz > 0.0 && pu(F_DEHAZE_V3) != 0u) {
            // process V3 (`finish::dehaze_v3_pixel`)
            let k0 = clamp(dz, 0.0, 1.0);
            let k = k0 * (0.4 + 0.6 * k0);
            let t = max(1.0 - 0.95 * k * d, 0.3);
            let veil = min(air * (1.0 - t), 0.9 * max(cmin, 0.0));
            c = max((c - veil) / t, vec3<f32>(0.0));
        } else if (dz > 0.0) {
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
        let wh = sstep(-2.4, 1.0, base) * (1.0 - 0.7 * sstep(1.4, 3.2, base));
        let sg = select(0.92, 0.82, ss > 0.0);
        delta += ss * 1.7 * sg * ws * sqrt(ws) + hh * 0.85 * wh;
    }
    // local Whites / Blacks, each mask on its own tone range (lcc_pipeline::finish::MASK_TONE,
    // LOCAL_WB_EV)
    if (lt[6] != 0.0 || lt[7] != 0.0) {
        for (var m = 0u; m < nm; m++) {
            let a = masks[m * mn + mi];
            if (a <= 0.0) {
                continue;
            }
            let t = pu(F_MASK_OFF) + m * MASK_TERMS;
            let wh = aux[t + 6u];
            let bl = aux[t + 7u];
            if (wh != 0.0) {
                let wt = sstep(aux[t + MASK_TONE], aux[t + MASK_TONE + 1u], l1);
                delta += a * wh * 1.5 * wt * sqrt(wt);
            }
            if (bl != 0.0) {
                let bt = 1.0 - sstep(aux[t + MASK_TONE + 2u], aux[t + MASK_TONE + 3u], l1);
                delta += a * bl * 1.5 * bt;
            }
        }
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
    let v4 = pu(F_SHARPEN_V4) != 0u;
    // V1-V3 sharpen by boosting the texture band; V4 has its own pixel-scale band
    var sp_old = 0.0;
    if (!v4) {
        sp_old = lt[13] * 0.6 + pf(F_SHARPEN);
    }
    if ((tx != 0.0 || sp_old != 0.0) && pu(F_HAS_TEX) != 0u) {
        let det = l_pre - tex[i];
        let tame = 1.0 - 0.6 * sstep(0.4, 1.6, abs(det));
        delta += tx * 1.1 * clamp(det, -1.0, 1.0) * tame;
        if (sp_old != 0.0) {
            let sm = pf(F_SHARPEN_MASK);
            var mk = 1.0;
            if (sm > 0.0) {
                mk = sstep(sm * 0.25, sm * 0.25 + 0.15, abs(det));
            }
            delta += sp_old * 1.3 * clamp(det, -0.8, 0.8) * mk;
        }
    }
    // V4 sharpening: unsharp mask on the pixel-scale band (`sharpen_term` on the CPU)
    var sp = 0.0;
    if (v4) {
        sp = (lt[13] * 0.6 + pf(F_SHARPEN)) * pf(F_SHARPEN_FADE);
    }
    if (sp != 0.0 && pu(F_HAS_SHP) != 0u) {
        let det = l_pre - tex[pu(F_SHP_OFF) + i];
        let sm = pf(F_SHARPEN_MASK);
        var mk = 1.0;
        if (sm > 0.0) {
            mk = sstep(sm * 0.1, sm * 0.1 + 0.06, abs(det));
        }
        delta += sp * 7.0 * mk * det / (1.0 + abs(det) / pf(F_SHARPEN_LIMIT));
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
    var o = tone_apply(yl);
    var d = vec3<f32>(0.0);
    if (yl > 1e-9) {
        d = c * o / yl;
    }
    let k = chroma_scale(o);
    if (k != 1.0) {
        d = vec3<f32>(o) + (d - vec3<f32>(o)) * k;
    }
    let peak = pf(F_PEAK);
    if (pu(F_RGB_CONTRAST) != 0u) {
        let rc_base = pu(F_RGB_CONTRAST_OFF);
        let rc_zero = pf(F_RGB_CONTRAST_ZERO);
        d = vec3<f32>(
            rgb_contrast_apply(d.x, rc_base, rc_zero, peak),
            rgb_contrast_apply(d.y, rc_base, rc_zero, peak),
            rgb_contrast_apply(d.z, rc_base, rc_zero, peak),
        );
        o = rgb_contrast_apply(o, rc_base, rc_zero, peak);
    }
    let hdr = peak > 1.0;
    let deep = pu(F_DEEP) != 0u;
    let mx = max(d.x, max(d.y, d.z));
    if (mx > peak) {
        let t = clamp((mx - peak) / max(mx - o, 1e-6), 0.0, 1.0);
        let grey = d + (o - d) * t;
        if (pu(F_HUE_CLIP) != 0u) {
            let kept = hue_clip(d, peak);
            d = kept + (grey - kept) * hue_fade(o, mx, peak);
        } else {
            d = grey;
        }
    }

    // --- primaries (the colourist wheels: lift/gamma/gain/offset, balance before the look)
    d = primaries(d);
    // --- colour
    d = color_ops(d, lt[11], lt[12]);
    if (pu(F_LPOINTS) != 0u) {
        d = local_point_colors(d, mi, mn);
    }
    if (tint_on) {
        let lab = oklab(d);
        d = oklab_inv(vec3<f32>(lab.x, lab.y + tint_dir.x * 0.08 * tint_amt, lab.z + tint_dir.y * 0.08 * tint_amt));
    }
    // colour grading inside masks (lcc_pipeline::colorops::local_grade): terms 17..29, the Color
    // Grading panel's zones at blending 50, balance 0
    var graded = false;
    for (var k = 17u; k < 29u; k++) {
        if (lt[k] != 0.0) {
            graded = true;
        }
    }
    if (graded) {
        var lab = oklab(d);
        let ws = 1.0 - sstep(0.1, 0.6, lab.x);
        let wh = sstep(0.4, 0.9, lab.x);
        let wm = max(1.0 - ws - wh, 0.0);
        let zw = array<f32, 4>(ws, wm, wh, 1.0);
        for (var z = 0u; z < 4u; z++) {
            lab.y += lt[17u + z * 3u] * zw[z];
            lab.z += lt[18u + z * 3u] * zw[z];
            lab.x += lt[19u + z * 3u] * zw[z];
        }
        d = oklab_inv(lab);
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
                d = d + max(vec3<f32>(1.0) - d, vec3<f32>(0.0)) * amount * t * 0.85;
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
        let py = vec3<f32>(pf(F_PROOF_Y), pf(F_PROOF_Y + 1u), pf(F_PROOF_Y + 2u));
        r = mul3(mat_at(F_PROOF_OUT), keep_hue_in_gamut(q0, gamut_apply(q0, py, tq, peak), py, peak));
    } else {
        r = mul3(mat_at(F_OUT_M), d);
    }
    let oy = vec3<f32>(pf(F_OUT_Y), pf(F_OUT_Y + 1u), pf(F_OUT_Y + 2u));
    let tg = gamut_scale(r, oy, peak);
    if (pu(F_PROOF) != 0u && pu(F_PROOF_DISP_WARN) != 0u && warn == 0u && out_of_gamut(r, tg)) {
        warn = 2u;
    }
    r = keep_hue_in_gamut(r, gamut_apply(r, oy, tg, peak), oy, peak);

    // --- encode, curves, grain
    var e: vec3<f32>;
    if (hdr) {
        // HDR: the (unbounded) sRGB curve, so values above SDR white encode above 1
        let q = max(r, vec3<f32>(0.0));
        e = vec3<f32>(lin_to_srgb(q.x), lin_to_srgb(q.y), lin_to_srgb(q.z));
    } else if (deep) {
        let q = clamp(r, vec3<f32>(0.0), vec3<f32>(1.0));
        e = vec3<f32>(lin_to_srgb(q.x), lin_to_srgb(q.y), lin_to_srgb(q.z));
    } else {
        e = vec3<f32>(encode_srgb(r.x), encode_srgb(r.y), encode_srgb(r.z));
    }
    // --- Brightness (`finish::finish_with`): right after the encode, before the Tone Curve
    if (pu(F_BRIGHTNESS) != 0u) {
        e = brightness_apply(e, hdr);
    }
    // --- Levels: each channel's own stage first, then the master's (one call site, as `finish_with`)
    if (pu(F_LEVELS) != 0u) {
        let c = vec3<f32>(
            levels_stage(F_LEVELS_R, pu(F_LEVELS_OFF_R), e.x, hdr),
            levels_stage(F_LEVELS_G, pu(F_LEVELS_OFF_G), e.y, hdr),
            levels_stage(F_LEVELS_B, pu(F_LEVELS_OFF_B), e.z, hdr));
        e = vec3<f32>(
            levels_stage(F_LEVELS_M, pu(F_LEVELS_OFF_M), c.x, hdr),
            levels_stage(F_LEVELS_M, pu(F_LEVELS_OFF_M), c.y, hdr),
            levels_stage(F_LEVELS_M, pu(F_LEVELS_OFF_M), c.z, hdr));
    }
    if (pu(F_CURVES) != 0u) {
        let e0 = e;
        e = vec3<f32>(curve(0u, e.x, hdr), curve(1u, e.y, hdr), curve(2u, e.z, hdr));
        let rs = pf(F_REFINE_SAT);
        if (rs < 1.0) {
            e = refine_saturation(e0, e, rs);
        }
    }
    // --- the masks' local Curves, in mask order, each weighted by its mask (`finish::local_curve_luts`)
    if (pu(F_LCURVES) != 0u) {
        let so = pu(F_LCURVE_OFF);
        for (var m = 0u; m < nm; m++) {
            let slot = aux[so + m];
            let a = masks[m * mn + mi];
            if (slot < 0.0 || a <= 0.0) {
                continue;
            }
            let o = so + nm + u32(slot) * 3u * CURVE_N;
            let c = vec3<f32>(lut_hdr(o, e.x, hdr), lut_hdr(o + CURVE_N, e.y, hdr), lut_hdr(o + 2u * CURVE_N, e.z, hdr));
            e = e + (c - e) * a;
        }
    }
    if (pu(F_GRAIN) != 0u) {
        let px = f32(x) + 0.5;
        let py = f32(y) + 0.5;
        let gx = pf(F_GRAIN_AFF) * px + pf(F_GRAIN_AFF + 2u) * py + pf(F_GRAIN_AFF + 4u);
        let gy = pf(F_GRAIN_AFF + 1u) * px + pf(F_GRAIN_AFF + 3u) * py + pf(F_GRAIN_AFF + 5u);
        if (pu(F_GRAIN_MODEL) == 0u) {
            // processes V1–V6: value noise (frozen)
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
        } else {
            // process V7: clumps of particles (`lcc_pipeline::grain`)
            let lum = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
            e = e + grain_apply(gx, gy, lum);
        }
    }
    // --- a LUT profile (`Lut3d::apply` on the SDR part, blended by the profile amount; in HDR the
    // part above SDR white is carried over)
    if (pu(F_LUT) != 0u) {
        let c = clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
        let m = lut_apply(c);
        if (hdr) {
            e = e + (m - c) * pf(F_LUT_AMT);
        } else {
            e = e + (m - e) * pf(F_LUT_AMT);
        }
    }
    if (warn == 1u) {
        e = vec3<f32>(1.0, 0.0, 0.0);
    } else if (warn == 2u) {
        e = vec3<f32>(0.0, 0.25, 1.0);
    }
    if (deep) {
        // linear in the output primaries (`finish::finish_deep`): up to `peak` (HDR) or 1
        let q = max(e, vec3<f32>(0.0));
        let lin = clamp(vec3<f32>(srgb_to_lin(q.x), srgb_to_lin(q.y), srgb_to_lin(q.z)), vec3<f32>(0.0), vec3<f32>(peak));
        let o = pu(F_DEEP_OFF) + 3u * i;
        out[o] = bitcast<u32>(lin.x);
        out[o + 1u] = bitcast<u32>(lin.y);
        out[o + 2u] = bitcast<u32>(lin.z);
        let sdr = clamp(lin, vec3<f32>(0.0), vec3<f32>(1.0));
        e = vec3<f32>(lin_to_srgb(sdr.x), lin_to_srgb(sdr.y), lin_to_srgb(sdr.z));
    }
    e = vec3<f32>(out_encode(e.x), out_encode(e.y), out_encode(e.z));
    var dither8 = 0.5;
    if (pu(F_DITHER) != 0u) {
        let ox = u32(max(floor(pf(F_VIG_VIEW)), 0.0));
        let oy = u32(max(floor(pf(F_VIG_VIEW + 1u)), 0.0));
        dither8 = bayer_dither(ox + x, oy + y);
    }
    out[i] = enc8(e.x, dither8) | (enc8(e.y, dither8) << 8u) | (enc8(e.z, dither8) << 16u) | (255u << 24u);
}

// `Lut3d::apply` (trilinear) on the LUT profile's table in `aux` (red fastest).
fn lut3_at(r: u32, g: u32, b: u32) -> vec3<f32> {
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
    let c00 = mix(lut3_at(r, g, b), lut3_at(r + 1u, g, b), fr.x);
    let c10 = mix(lut3_at(r, g + 1u, b), lut3_at(r + 1u, g + 1u, b), fr.x);
    let c01 = mix(lut3_at(r, g, b + 1u), lut3_at(r + 1u, g, b + 1u), fr.x);
    let c11 = mix(lut3_at(r, g + 1u, b + 1u), lut3_at(r + 1u, g + 1u, b + 1u), fr.x);
    return mix(mix(c00, c10, fr.y), mix(c01, c11, fr.y), fr.z);
}

// `finish::hue_clip` (process V5): the brightest channel limited to `peak`, the darkest kept (all three `peak` when it
// is above too), the middle one in its place between them
fn hue_clip(c: vec3<f32>, peak: f32) -> vec3<f32> {
    // Rust's `f32::max` / `min` skip a NaN operand; WGSL's are undefined for it, so do it by hand
    let hi = nan_max(c.x, nan_max(c.y, c.z));
    if (is_nan(hi) || !(hi > peak) || !is_finite(peak)) {
        return c;
    }
    let lo = nan_min(c.x, nan_min(c.y, c.z));
    if (lo >= peak || !is_finite(lo)) {
        return vec3<f32>(peak);
    }
    return vec3<f32>(lo) + (c - vec3<f32>(lo)) * ((peak - lo) / (hi - lo));
}

// NaN and infinity tests on the bits (a compiler may fold `v != v` away)
fn is_nan(v: f32) -> bool {
    let b = bitcast<u32>(v);
    return (b & 0x7f800000u) == 0x7f800000u && (b & 0x007fffffu) != 0u;
}
fn is_finite(v: f32) -> bool {
    return (bitcast<u32>(v) & 0x7f800000u) != 0x7f800000u;
}
fn nan_max(a: f32, b: f32) -> f32 {
    return select(select(max(a, b), a, is_nan(b)), b, is_nan(a));
}
fn nan_min(a: f32, b: f32) -> f32 {
    return select(select(min(a, b), a, is_nan(b)), b, is_nan(a));
}

// `finish::HUE_FADE_FROM` and `HUE_FADE_EV` (keep in step)
const HUE_FADE_FROM: f32 = 0.35;
const HUE_FADE_EV: f32 = 0.5;

// `finish::hue_fade`
fn hue_fade(y: f32, hi: f32, peak: f32) -> f32 {
    let over = select(0.0, log2(hi / peak), hi > peak && peak > 0.0);
    return max(sstep(0.0, HUE_FADE_EV, over), sstep(HUE_FADE_FROM * peak, peak, y));
}

// `finish::keep_hue_in_gamut`, or `mapped` itself before process V5
fn keep_hue_in_gamut(r: vec3<f32>, mapped: vec3<f32>, luma: vec3<f32>, peak: f32) -> vec3<f32> {
    if (pu(F_HUE_CLIP) == 0u) {
        return mapped;
    }
    let c = hue_clip(r, peak);
    // `finish::lift_negatives`
    let yy = clamp(dot(luma, c), 0.0, peak);
    var t = 1.0;
    for (var k = 0u; k < 3u; k++) {
        if (c[k] < 0.0) {
            t = min(t, yy / max(yy - c[k], 1e-9));
        }
    }
    var kept = c;
    if (t < 1.0) {
        kept = yy + (c - yy) * t;
    }
    return kept + (mapped - kept) * hue_fade(dot(luma, r), max(r.x, max(r.y, r.z)), peak);
}
