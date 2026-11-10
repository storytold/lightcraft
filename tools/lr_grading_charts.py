#!/usr/bin/env python3
"""Measure Lightroom's Colour Grading on synthetic charts (process V8, docs/grading-stage.md).

    python3 tools/lr_grading_charts.py charts OUT_DIR
        Writes linear DNG charts (the grey steps and colour patches of tools/lr_tone_charts.py) with the settings
        embedded as XMP: one neutral chart; each wheel (shadows, midtones, highlights, global) at 12 hues x 3
        saturations, at Luminance -100/-50/50/100, at Blending 0/25/75/100 and (not global) at Balance
        -100/-50/50/100; and two combinations used only as checks.
    (render every DNG through Camera Raw to a 16-bit Adobe RGB (1998) TIFF with the same file name)
    python3 tools/lr_grading_charts.py tables RENDER_DIR OUT_RS
        Reads the renders and writes the Rust tables of `lightcraft_pipeline::grading_stage`.

The charts are original test data generated here; nothing in them comes from Adobe. Only Camera Raw's output is
measured. Requires numpy and tifffile (tools/requirements-lr-tone-charts.txt).

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


def tables(render_dir, out_rs):
    patches = json.load(open(os.path.join(render_dir, "manifest.json")))["patches"]
    ok = np.array([p["ok"] for p in patches])
    grey = ok & np.array([p["kind"] == "grey" for p in patches])
    col = ok & np.array([p["kind"] == "colour" for p in patches])
    cache = {}

    def lab(n):
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

    f = lambda v, p=6: f"{v:.{p}f}"
    row = lambda vs, p=6: "[" + ", ".join(f(v, p) for v in vs) + "]"
    names = list(WHEELS)
    s = ["// @generated by tools/lr_grading_charts.py from Camera Raw renders of its synthetic charts (docs/grading-stage.md).",
         "// OkLab. Wheels: shadows, midtones, highlights, global. Lightness grids: 65 steps from 0 to 1.",
         "// Measured values, which can come close to a mathematical constant by chance.",
         "#![allow(clippy::approx_constant)]",
         "",
         f"pub const HUES: [f32; {len(HUES)}] = [{', '.join(f'{h}.0' for h in HUES)}];",
         f"pub const SATS: [f32; {len(SATS)}] = [{', '.join(f'{x}.0' for x in SATS)}];",
         f"pub const BLENDS: [f32; {len(BLENDS)}] = [{', '.join(f'{x}.0' for x in BLENDS)}];",
         f"pub const BALANCES: [f32; {len(BALANCES)}] = [{', '.join(f'{x}.0' for x in BALANCES)}];",
         f"pub const LUMS: [f32; {len(LUMS)}] = [{', '.join(f'{x}.0' for x in LUMS)}];",
         f"/// Nominal chroma of the damping's levels.",
         f"pub const CHROMA: [f32; 3] = [{', '.join(str(c) for c in CHROMA)}];",
         "",
         "/// Tint (a, b) per wheel, hue and saturation.",
         f"pub static TINT: [[[[f32; 2]; {len(SATS)}]; {len(HUES)}]; 4] = ["]
    for w in names:
        s.append("    [")
        for h in HUES:
            s.append("        [" + ", ".join(row(tint[(w, h, x)]) for x in SATS) + "],")
        s.append("    ],")
    s += ["];", "",
          "/// Share of the tint by lightness, per wheel and Blending (at Balance 0).",
          f"pub static PROFILE_BLEND: [[[f32; {GRID}]; {len(BLENDS)}]; 4] = ["]
    for w in names:
        s += ["    ["] + [f"        {row(pblend[(w, b)], 5)}," for b in BLENDS] + ["    ],"]
    s += ["];", "",
          "/// Share of the tint by lightness, per wheel and Balance (at Blending 50; the global wheel ignores Balance).",
          f"pub static PROFILE_BALANCE: [[[f32; {GRID}]; {len(BALANCES)}]; 4] = ["]
    for w in names:
        s += ["    ["] + [f"        {row(pbal[(w, a)], 5)}," for a in BALANCES] + ["    ],"]
    s += ["];", "",
          "/// Lightness change by lightness, per wheel and Luminance.",
          f"pub static LUM: [[[f32; {GRID}]; {len(LUMS)}]; 4] = ["]
    for w in names:
        s += ["    ["] + [f"        {row(lum[(w, v)])}," for v in LUMS] + ["    ],"]
    s += ["];", "",
          "/// Share of the tint a colour gets, per wheel, chroma level and angle between its hue and the tint's (30 degree bins).",
          "pub static DAMPING: [[[f32; 6]; 3]; 4] = ["]
    for w in names:
        s += ["    ["] + [f"        {row(att[w][i], 4)}," for i in range(3)] + ["    ],"]
    s += ["];", ""]
    with open(out_rs, "w") as fh:
        fh.write("\n".join(s))


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "charts":
        out = sys.argv[2]
        os.makedirs(out, exist_ok=True)
        img, patches = chart()
        json.dump(dict(patches=patches), open(os.path.join(out, "manifest.json"), "w"))
        for name, settings in variants().items():
            write_dng(os.path.join(out, f"grade__{name}.dng"), img, xmp(**settings))
    elif len(sys.argv) == 4 and sys.argv[1] == "tables":
        tables(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
