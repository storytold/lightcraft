#!/usr/bin/env python3
"""Measure Lightroom's Colour Grading on synthetic charts (process V9, docs/grading-stage.md).

    python3 tools/lr_grading_charts.py charts OUT_DIR
        Writes linear DNG charts (the grey steps and colour patches of tools/lr_tone_charts.py) with the settings
        embedded as XMP: one neutral chart; each wheel (shadows, midtones, highlights, global) at 12 hues x 3
        saturations, at Luminance -100/-50/50/100, at Blending 0/25/75/100 and (not global) at Balance
        -100/-50/50/100; and two combinations used only as checks.
    (render every DNG through Camera Raw to a 16-bit Adobe RGB (1998) TIFF with the same file name)
    python3 tools/lr_grading_charts.py fit RENDER_DIR OUT_RS
        Reads the renders, fits LightCraft's model of Colour Grading to them and writes the model's constants
        (`lightcraft_pipeline::grading_stage_model`). The measurements themselves are not written out.

The charts are original test data generated here; nothing in them comes from Adobe. Only Camera Raw's output is
measured. Requires numpy, scipy and tifffile (tools/requirements-lr-tone-charts.txt).

The model, per wheel: an OkLab tint (a, b) by wheel hue and saturation; a lightness profile (how much of the tint
each lightness gets) by Blending and Balance; a damping of the tint for colours that already lean its way, by their
chroma and the angle between their hue and the tint's; and the wheel's Luminance as a lightness change by lightness.
"""
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lr_tone_charts import ADOBE_TO_XYZ, chart, write_dng, xmp  # noqa: E402

WHEELS = {
    "shadows": ("SplitToningShadowHue", "SplitToningShadowSaturation", "ColorGradeShadowLum"),
    "midtones": ("ColorGradeMidtoneHue", "ColorGradeMidtoneSat", "ColorGradeMidtoneLum"),
    "highlights": ("SplitToningHighlightHue", "SplitToningHighlightSaturation", "ColorGradeHighlightLum"),
    "global": ("ColorGradeGlobalHue", "ColorGradeGlobalSat", "ColorGradeGlobalLum"),
}
HUES = tuple(range(0, 360, 30))
SATS = (25, 50, 100)
BLENDS = (0, 25, 50, 75, 100)
BALANCES = (-100, -50, 0, 50, 100)
LUMS = (-100, -50, 50, 100)
CHROMA = (0.04, 0.08, 0.12)
GRID = 65
# the hue and saturation each wheel is measured at for Blending and Balance
REF_HUE, REF_SAT = 30, 100

M1 = np.array([[0.8189330101, 0.3618667424, -0.1288597137], [0.0329845436, 0.9293118715, 0.0361456387],
               [0.0482003018, 0.2643662691, 0.6338517070]])
M2 = np.array([[0.2104542553, 0.793617785, -0.0040720468], [1.9779984951, -2.428592205, 0.4505937099],
               [0.0259040371, 0.7827717662, -0.808675766]])


def variants():
    """Name and XMP settings of every chart (neutral, wheels, Luminance, Blending, Balance, checks)."""
    v = {"neutral": {}}
    for w, (kh, ks, kl) in WHEELS.items():
        for h in HUES:
            for s in SATS:
                v[f"{w}_h{h}_s{s}"] = {kh: h, ks: s, "ColorGradeBlending": 50}
        for lum in (-50, 50):
            v[f"{w}_lum{lum:+d}"] = {kl: lum, "ColorGradeBlending": 50}
    split = {"SplitToningShadowHue": 52, "SplitToningShadowSaturation": 50, "SplitToningHighlightHue": 220,
             "SplitToningHighlightSaturation": 50}
    for b in (0, 25, 75, 100):
        v[f"blend{b}"] = {**split, "ColorGradeBlending": b}
    for bal in (-50, 50):
        v[f"balance{bal:+d}"] = {**split, "ColorGradeBlending": 50, "SplitToningBalance": bal}
    v["dm_grading"] = {"SplitToningShadowHue": 52, "SplitToningShadowSaturation": 20, "SplitToningHighlightHue": 34,
                       "SplitToningHighlightSaturation": 0, "SplitToningBalance": 40, "ColorGradeBlending": 100}
    v["to_grading"] = {"SplitToningShadowHue": 205, "SplitToningShadowSaturation": 18, "SplitToningHighlightHue": 46,
                       "SplitToningHighlightSaturation": 80, "ColorGradeMidtoneHue": 45, "ColorGradeMidtoneSat": 22,
                       "ColorGradeGlobalHue": 46, "ColorGradeGlobalSat": 33, "ColorGradeBlending": 50}
    for w, (kh, ks, kl) in WHEELS.items():
        for b in (0, 25, 75, 100):
            v[f"{w}_blend{b}"] = {kh: REF_HUE, ks: REF_SAT, "ColorGradeBlending": b}
        if w != "global":
            for bal in (-100, -50, 50, 100):
                v[f"{w}_bal{bal:+d}"] = {kh: REF_HUE, ks: REF_SAT, "ColorGradeBlending": 50, "SplitToningBalance": bal}
        for lum in (-100, 100):
            v[f"{w}_lum{lum:+d}"] = {kl: lum, "ColorGradeBlending": 50}
    return v


# ---- tables -------------------------------------------------------------------------------------------------

def oklab_patches(path, patches):
    """Mean colour of every patch of a Camera Raw render, as OkLab."""
    import tifffile
    a = tifffile.imread(path).astype(np.float64)
    a /= 65535.0 if a.max() > 255 else 255.0
    out = []
    for q in patches:
        rgb = a[q["y"] + 8:q["y"] + q["h"] - 8, q["x"] + 8:q["x"] + q["w"] - 8].reshape(-1, 3).mean(0)
        xyz = ADOBE_TO_XYZ @ (np.clip(rgb, 0, 1) ** (563 / 256))
        out.append(M2 @ np.cbrt(np.maximum(M1 @ xyz, 0)))
    return np.array(out)


def measure(render_dir):
    """Measure the Camera Raw renders in `render_dir`: tints, profiles by Blending and by Balance, Luminance curves and
    damping per wheel, as dictionaries (kept in memory, never written out)."""
    patches = json.load(open(os.path.join(render_dir, "manifest.json")))["patches"]
    ok = np.array([p["ok"] for p in patches])
    grey = ok & np.array([p["kind"] == "grey" for p in patches])
    col = ok & np.array([p["kind"] == "colour" for p in patches])
    cache = {}

    def lab(n):
        """OkLab patches of render `n`, read once."""
        if n not in cache:
            cache[n] = oklab_patches(os.path.join(render_dir, f"grade__{n}.tif"), patches)
        return cache[n]

    n0 = lab("neutral")
    order = np.argsort(n0[grey][:, 0])
    grey_l = n0[grey][order, 0]
    grid = np.linspace(0, 1, GRID)

    def offsets(n):
        """(dL, da, db) of the grey steps against neutral, on the lightness grid; towards 0 below the darkest step."""
        d = (lab(n) - n0)[grey][order]
        out = np.stack([np.interp(grid, grey_l, d[:, k]) for k in range(3)], 1)
        low = grid < grey_l[0]
        out[low] *= (grid[low] / grey_l[0])[:, None]
        return out

    chroma = np.array([p.get("C", 0) for p in patches])[col]
    nc = n0[col]
    hue_c = np.arctan2(nc[:, 2], nc[:, 1])
    tint, pblend, pbal, lum, att = {}, {}, {}, {}, {}
    for w in WHEELS:
        # the profile's shape at Blending 50: mean tint size over the hues at saturation 100, peak 1
        size = np.mean([np.hypot(*offsets(f"{w}_h{h}_s100")[:, 1:].T) for h in HUES], 0)
        prof = size / size.max()
        for h in HUES:
            for s in SATS:
                tint[(w, h, s)] = (offsets(f"{w}_h{h}_s{s}") * prof[:, None]).sum(0)[1:] / (prof ** 2).sum()
        ref = np.hypot(*tint[(w, REF_HUE, REF_SAT)])
        ref_name = f"{w}_h{REF_HUE}_s{REF_SAT}"
        for b in BLENDS:
            pblend[(w, b)] = np.hypot(*offsets(ref_name if b == 50 else f"{w}_blend{b}")[:, 1:].T) / ref
        for a in BALANCES:
            if w == "global":
                pbal[(w, a)] = pblend[(w, 50)]
            else:
                pbal[(w, a)] = np.hypot(*offsets(ref_name if a == 0 else f"{w}_bal{a:+d}")[:, 1:].T) / ref
        for v in LUMS:
            lum[(w, v)] = offsets(f"{w}_lum{v:+d}")[:, 0]
        # damping: the colour patches' change along the grey steps' tint, by chroma level and angle (6 x 30 degrees)
        acc, cnt = np.zeros((3, 6)), np.zeros((3, 6))
        for h in HUES:
            name = f"{w}_h{h}_s100"
            d = (lab(name) - n0)[col]
            g = offsets(name)
            gd = np.stack([np.interp(nc[:, 0], grid, g[:, k]) for k in range(3)], 1)
            mag = np.hypot(gd[:, 1], gd[:, 2])
            m = mag > 0.01
            u = gd[:, 1:] / np.maximum(mag, 1e-9)[:, None]
            proj = (d[:, 1:] * u).sum(1) / np.maximum(mag, 1e-9)
            ang = np.degrees(np.abs((hue_c - np.arctan2(gd[:, 2], gd[:, 1]) + np.pi) % (2 * np.pi) - np.pi))
            for i, c in enumerate(CHROMA):
                for j in range(6):
                    k = m & (chroma == c) & (ang >= j * 30) & (ang < j * 30 + 30)
                    if k.any():
                        acc[i, j] += np.median(proj[k])
                        cnt[i, j] += 1
        att[w] = np.where(cnt > 0, acc / np.maximum(cnt, 1), 1.0)

    return tint, pblend, pbal, lum, att


# ---- fit ----------------------------------------------------------------------------------------------------
#
# LightCraft's own model of Colour Grading, fitted to the measurements above. Nothing measured is written out: only
# the model's constants (docs/grading-stage.md has the formulas).
#
# - Tint: the same shape around the hue circle for every wheel, a Fourier series of the wheel hue for the tint's
#   direction (offset from the wheel hue) and for its log size, times a size per wheel and a saturation curve per
#   wheel: size(s) = s (1 + g1 (s - 1) + g2 (s - 1)^2), s = saturation / 100.
# - Share by lightness: two bumps (as the tone curve stage's), whose gain and exponents are quadratic in Blending and
#   in Balance.
# - Luminance: two bumps per direction, gain and exponents changing with the amount.
# - Damping: 1 + (C / 0.12)^e (d0 + d1 cos a + d2 cos 2a), a the angle between the colour's hue and the tint's.

HARMONICS = 4
FIT_SEED = 3
FIT_STARTS = 24


def fourier(c, h):
    """c0 + sum of c[2k-1] cos(k h) + c[2k] sin(k h)."""
    return c[0] + sum(c[2 * k - 1] * np.cos(k * h) + c[2 * k] * np.sin(k * h) for k in range(1, (len(c) - 1) // 2 + 1))


def bump(x, p, q):
    """A bump of peak 1 at p / (p + q), 0 at black and white."""
    m = p / (p + q)
    return np.exp(p * np.log(np.clip(x, 1e-12, 1) / m) + q * np.log(np.clip(1 - x, 1e-12, 1) / (1 - m)))


def tint_model(params, hue, sat):
    """Tints (wheel, hue, saturation, a/b) for hues in radians and saturations 0..1."""
    n = 2 * HARMONICS + 1
    angle = hue + fourier(params[:n], hue)
    size = np.exp(fourier(params[n:2 * n], hue))
    out = np.zeros((4, len(hue), len(sat), 2))
    for w in range(4):
        scale, g1, g2 = np.exp(params[2 * n + w]), params[2 * n + 4 + 2 * w], params[2 * n + 5 + 2 * w]
        for i, s in enumerate(sat):
            m = scale * size * s * (1 + g1 * (s - 1) + g2 * (s - 1) ** 2)
            out[w, :, i] = np.stack([m * np.cos(angle), m * np.sin(angle)], 1)
    return out


def share_model(params, blend, balance, light):
    """Share of the tint at lightness `light` for Blending and Balance as fractions (Blending 0.5 and Balance 0 at the default)."""
    b, c = blend - 0.5, balance
    out = 0.0
    for k in range(2):
        q = params[15 * k:15 * k + 15]
        f = lambda i: q[i] + q[i + 1] * b + q[i + 2] * b * b + q[i + 3] * c + q[i + 4] * c * c
        out = out + f(0) * bump(light, np.exp(f(5)), np.exp(f(10)))
    return out


def lum_model(params, amount, light):
    """Lightness change at `light` for Luminance `amount` (-1..1)."""
    a, side = abs(amount), 0 if amount < 0 else 1
    out = 0.0
    for k in range(2):
        q = params[12 * side + 6 * k:12 * side + 6 * k + 6]
        out = out + (q[0] * a + q[1] * a * a) * bump(light, np.exp(q[2] + q[3] * a), np.exp(q[4] + q[5] * a))
    return np.sign(amount) * out


def damping_model(params, chroma, angle):
    """Share of the tint a colour of `chroma` gets at `angle` (radians) from the tint's direction."""
    d0, d1, d2, e = params
    return 1 + (chroma / 0.12) ** np.exp(e) * (d0 + d1 * np.cos(angle) + d2 * np.cos(2 * angle))


def multistart(residual, starts):
    """The best least-squares solution over `starts`."""
    from scipy.optimize import least_squares
    return min((least_squares(residual, s, max_nfev=4000) for s in starts), key=lambda r: r.cost).x


def fit_share(job):
    """One wheel's share profiles (by Blending, by Balance) on the lightness grid."""
    blends, balances, light = job
    def residual(p):
        return np.concatenate([share_model(p, b, 0.0, light) - v for b, v in blends] +
                              [share_model(p, 0.5, a, light) - v for a, v in balances]) * 100
    rng = np.random.default_rng(FIT_SEED)
    starts = []
    for _ in range(FIT_STARTS):
        s = np.zeros(30)
        for k in range(2):
            s[15 * k], s[15 * k + 5], s[15 * k + 10] = rng.uniform(0.2, 1), rng.uniform(-1, 2.5), rng.uniform(-1, 2.5)
        starts.append(s)
    return multistart(residual, starts)


def fit_lum(job):
    """One wheel's Luminance responses on the lightness grid."""
    lums, light = job
    residual = lambda p: np.concatenate([lum_model(p, a, light) - v for a, v in lums]) * 100
    rng = np.random.default_rng(FIT_SEED)
    starts = []
    for _ in range(FIT_STARTS):
        s = np.zeros(24)
        for side in range(2):
            for k in range(2):
                o = 12 * side + 6 * k
                s[o], s[o + 2], s[o + 4] = rng.uniform(0, 0.1), rng.uniform(-1, 2.5), rng.uniform(-1, 2.5)
        starts.append(s)
    return multistart(residual, starts)


def fit_damping(table):
    """One wheel's damping (chroma level x 6 angle bins) as 4 constants."""
    angle = np.radians(np.arange(15, 180, 30))
    c = np.array(CHROMA)[:, None]
    return multistart(lambda p: (damping_model(p, c, angle[None, :]) - table).ravel() * 100, [np.zeros(4)])


def fit(render_dir, out_rs):
    """Fit the model to the Camera Raw renders in `render_dir` and write its constants to `out_rs`."""
    from concurrent.futures import ProcessPoolExecutor
    tint, pblend, pbal, lum, att = measure(render_dir)
    names = list(WHEELS)
    light = np.linspace(0, 1, GRID)
    hue, sat = np.radians(np.array(HUES, float)), np.array(SATS) / 100
    measured = np.array([[[tint[(w, h, s)] for s in SATS] for h in HUES] for w in names])
    n = 2 * HARMONICS + 1
    start = np.zeros(2 * n + 12)
    start[n] = np.log(np.hypot(*measured.reshape(-1, 2).T).mean())
    tp = multistart(lambda p: ((tint_model(p, hue, sat) - measured) * 100).ravel(), [start])
    e = np.linalg.norm(tint_model(tp, hue, sat) - measured, axis=-1) * 100
    print(f"tint: rms {np.sqrt((e ** 2).mean()):.3f} of {np.linalg.norm(measured, axis=-1).mean() * 100:.3f} (OkLab x 100)")
    share_jobs = [([(b / 100, pblend[(w, b)]) for b in BLENDS], [] if w == "global" else [(a / 100, pbal[(w, a)]) for a in BALANCES], light)
                  for w in names]
    lum_jobs = [([(v / 100, lum[(w, v)]) for v in LUMS], light) for w in names]
    with ProcessPoolExecutor() as pool:
        shares = list(pool.map(fit_share, share_jobs))
        lums = list(pool.map(fit_lum, lum_jobs))
    damps = [fit_damping(att[w]) for w in names]
    for i, w in enumerate(names):
        blends, balances, _ = share_jobs[i]
        es = np.concatenate([share_model(shares[i], b, 0.0, light) - v for b, v in blends] +
                            [share_model(shares[i], 0.5, a, light) - v for a, v in balances]) * 100
        el = np.concatenate([lum_model(lums[i], a, light) - v for a, v in lum_jobs[i][0]]) * 100
        ed = (damping_model(damps[i], np.array(CHROMA)[:, None], np.radians(np.arange(15, 180, 30))[None, :]) - att[w]) * 100
        print(f"{w:10s} share rms {np.sqrt((es ** 2).mean()):.2f} (x 100), Luminance rms {np.sqrt((el ** 2).mean()):.3f} (OkLab L x 100), "
              f"damping rms {np.sqrt((ed ** 2).mean()):.2f} (x 100)")
    num = lambda v: f"{v:.6e}"
    row = lambda vs: "[" + ", ".join(num(v) for v in vs) + "]"
    s = ["// Written by `tools/lr_grading_charts.py fit`: LightCraft's model of Colour Grading, fitted to the tool's charts",
         "// (provenance in docs/grading-stage.md). Constants of the model only. Wheels: shadows, midtones, highlights, global.",
         "",
         "/// The tint's direction around the hue circle, as an offset from the wheel hue (radians): a Fourier series of the",
         "/// wheel hue, `c0 + c1 cos h + c2 sin h + c3 cos 2h + ...`.",
         f"pub const TINT_ANGLE: [f64; {n}] = {row(tp[:n])};",
         "/// The tint's log size around the hue circle, the same kind of series.",
         f"pub const TINT_SIZE: [f64; {n}] = {row(tp[n:2 * n])};",
         "/// Each wheel's log size, added to [`TINT_SIZE`].",
         f"pub const WHEEL_SIZE: [f64; 4] = {row(tp[2 * n:2 * n + 4])};",
         "/// Each wheel's saturation curve `[g1, g2]`: size `s (1 + g1 (s - 1) + g2 (s - 1)²)` at saturation `s` (0 to 1).",
         "pub const WHEEL_SAT: [[f64; 2]; 4] = [" + ", ".join(row(tp[2 * n + 4 + 2 * w:2 * n + 6 + 2 * w]) for w in range(4)) + "];",
         "/// Each wheel's share of the tint by lightness: two bumps of 15 constants, `[g, gb, gb2, gc, gc2, p, pb, ..., q, ...]`,",
         "/// each value quadratic in Blending (`b` = Blending / 100 - 0.5) and Balance (`c` = Balance / 100).",
         "pub const SHARE: [[f64; 30]; 4] = [",
         *[f"    {row(p)}," for p in shares],
         "];",
         "/// Each wheel's Luminance, negative then positive: two bumps of `[g1, g2, p0, p1, q0, q1]` each.",
         "pub const LUM: [[f64; 24]; 4] = [",
         *[f"    {row(p)}," for p in lums],
         "];",
         "/// Each wheel's damping `[d0, d1, d2, e]`: `1 + (C / 0.12)^(e^e) (d0 + d1 cos a + d2 cos 2a)`.",
         "pub const DAMPING: [[f64; 4]; 4] = [" + ", ".join(row(p) for p in damps) + "];",
         ""]
    with open(out_rs, "w") as f:
        f.write("\n".join(s))


def main():
    """Command line: `charts OUT_DIR` or `fit RENDER_DIR OUT_RS`."""
    if len(sys.argv) == 3 and sys.argv[1] == "charts":
        out = sys.argv[2]
        os.makedirs(out, exist_ok=True)
        img, patches = chart()
        json.dump(dict(patches=patches), open(os.path.join(out, "manifest.json"), "w"))
        for name, settings in variants().items():
            write_dng(os.path.join(out, f"grade__{name}.dng"), img, xmp(**settings))
    elif len(sys.argv) == 4 and sys.argv[1] == "fit":
        fit(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
