#!/usr/bin/env python3
"""Measure Lightroom's colour mixer on synthetic colour charts (process V9, docs/mixer-stage.md).

    python3 tools/lr_mixer_charts.py charts OUT_DIR
        Writes linear DNG charts (OkLCh patches: 72 hues x 3 chroma x 3 lightness levels) with the settings embedded
        as XMP: one neutral chart and every mixer band's Hue, Saturation and Luminance and Calibration's red, green
        and blue Hue and Saturation at -100, -75, -50, -25, 25, 50, 75 and 100.
    (render every DNG through Camera Raw to a 16-bit Adobe RGB (1998) TIFF with the same file name)
    python3 tools/lr_mixer_charts.py fit RENDER_DIR OUT_RS
        Reads the renders, fits LightCraft's model of each mixer slider to them and writes the model's constants
        (`lightcraft_pipeline::mixer_stage_model`). The measurements themselves are not written out.

The charts are original test data generated here; nothing in them comes from Adobe. Only Camera Raw's output is
measured. Requires numpy, scipy and tifffile (tools/requirements-lr-tone-charts.txt).
"""
import json
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lr_tone_charts import ADOBE_TO_XYZ, XYZ_TO_PROPHOTO, H, W, write_dng, xmp  # noqa: E402

CELL = 60
BANDS = ("Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta")
MIXER = (("Hue", "HueAdjustment"), ("Sat", "SaturationAdjustment"), ("Lum", "LuminanceAdjustment"))
AMOUNTS = (-100, -75, -50, -25, 25, 50, 75, 100)
LIGHTNESS = (0.45, 0.62, 0.78)
CHROMA = (0.04, 0.08, 0.13)
HUES = 72

# OkLab (Ottosson): XYZ (D65) -> LMS, cube root, -> Lab
M1 = np.array([[0.8189330101, 0.3618667424, -0.1288597137], [0.0329845436, 0.9293118715, 0.0361456387],
               [0.0482003018, 0.2643662691, 0.6338517070]])
M2 = np.array([[0.2104542553, 0.793617785, -0.0040720468], [1.9779984951, -2.428592205, 0.4505937099],
               [0.0259040371, 0.7827717662, -0.808675766]])
SRGB_TO_XYZ = np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]])


def chart():
    """OkLCh patches, hue in 5 degree steps x 3 chroma x 3 lightness levels; patches outside sRGB are left out."""
    m2i, m1i, x2s = np.linalg.inv(M2), np.linalg.inv(M1), np.linalg.inv(SRGB_TO_XYZ)
    cols = W // CELL
    img = np.full((H, W, 3), 0.18)
    patches = []
    for lightness in LIGHTNESS:
        for chroma in CHROMA:
            for hue in range(0, 360, 360 // HUES):
                i = len(patches)
                x, y = (i % cols) * CELL, (i // cols) * CELL
                h = np.radians(hue)
                c = x2s @ (m1i @ ((m2i @ np.array([lightness, chroma * np.cos(h), chroma * np.sin(h)])) ** 3))
                ok = bool(np.all(c >= 0) and np.all(c <= 1))
                if ok:
                    img[y + 6:y + CELL - 6, x + 6:x + CELL - 6] = c
                patches.append(dict(y=y + 6, x=x + 6, w=CELL - 12, h=CELL - 12, L=lightness, C=chroma, hue=hue, ok=ok))
    return img, patches


CALIBRATION = ("RedHue", "RedSaturation", "GreenHue", "GreenSaturation", "BlueHue", "BlueSaturation")


def sliders():
    """(chart name stem, XMP key) per slider, in the table's order: mixer bands x (Hue, Sat, Lum), then the
    calibration primaries x (Hue, Saturation)."""
    return [(f"mix_{b}{p}", k + b) for b in BANDS for p, k in MIXER] + [(f"cal_{k}", k) for k in CALIBRATION]


def variants():
    """Name and XMP settings of every chart: neutral, then each mixer slider at each amount."""
    v = {"neutral": {}}
    for stem, key in sliders():
        for a in AMOUNTS:
            v[f"{stem}{a:+d}"] = {key: a}
    return v


# ---- table --------------------------------------------------------------------------------------------------

def rgb_patches(path, patches):
    """Mean encoded Adobe RGB of each in-gamut patch of a Camera Raw render, 0..1."""
    import tifffile
    a = tifffile.imread(path).astype(np.float64)
    a /= 65535.0 if a.max() > 255 else 255.0
    return np.array([a[q["y"] + 10:q["y"] + q["h"] - 10, q["x"] + 10:q["x"] + q["w"] - 10].reshape(-1, 3).mean(0) for q in patches])


def oklab_patches(path, patches):
    """Mean colour of each in-gamut patch of a Camera Raw render, as OkLab."""
    import tifffile
    a = tifffile.imread(path).astype(np.float64)
    a /= 65535.0 if a.max() > 255 else 255.0
    out = []
    for q in patches:
        rgb = a[q["y"] + 10:q["y"] + q["h"] - 10, q["x"] + 10:q["x"] + q["w"] - 10].reshape(-1, 3).mean(0)
        xyz = ADOBE_TO_XYZ @ (np.clip(rgb, 0, 1) ** (563 / 256))
        out.append(M2 @ np.cbrt(M1 @ xyz))
    return np.array(out)


def by_hue(hue, values):
    """Mean of `values` per 5 degree hue bin, bins without a patch interpolated around the circle; and which bins had one."""
    bins = np.arange(0, 360, 360 // HUES)
    centres, means, have = [], [], np.zeros(HUES, bool)
    for i, b in enumerate(bins):
        k = ((hue - b + 2.5) % 360) < 5
        if k.any():
            centres.append(b)
            means.append(values[k].mean(0))
            have[i] = True
    centres, means = np.array(centres, float), np.array(means)
    return np.stack([np.interp(bins, centres, means[:, o], period=360) for o in range(3)], 1), have


def response(lab0, lab, light, chroma):
    """Hue shift (degrees), relative chroma change and lightness change of each chart setting against neutral,
    per lightness and chroma level and hue bin: shape (9, 3, 72)."""
    hue0 = np.degrees(np.arctan2(lab0[:, 2], lab0[:, 1])) % 360
    c0 = np.hypot(lab0[:, 1], lab0[:, 2])
    hue = np.degrees(np.arctan2(lab[:, 2], lab[:, 1])) % 360
    r = np.stack([(hue - hue0 + 180) % 360 - 180, np.hypot(lab[:, 1], lab[:, 2]) / c0 - 1, lab[:, 0] - lab0[:, 0]], 1)
    out = np.zeros((9, 3, HUES))
    for ci, cv in enumerate(CHROMA):
        m = chroma == cv
        base, _ = by_hue(hue0[m], r[m])
        for li, lv in enumerate(LIGHTNESS):
            m2 = m & (light == lv)
            if m2.sum() < 6:
                out[li * 3 + ci] = base.T
                continue
            c, have = by_hue(hue0[m2], r[m2])
            # hue bins without an in-gamut patch at this lightness: blend smoothly towards the all-lightness curve
            w = np.convolve(np.r_[have[-3:], have, have[:3]].astype(float), np.ones(7) / 7, "same")[3:-3]
            out[li * 3 + ci] = (c * w[:, None] + base * (1 - w[:, None])).T
    return out


# ---- fit ----------------------------------------------------------------------------------------------------
#
# LightCraft's own model of the colour mixer, fitted to the measured chart responses. Nothing measured is written
# out: only the model's 168 constants. For slider `kind` (Hue, Saturation, Luminance) of band `b` and output `o` (hue
# shift in degrees, relative chroma change, lightness change), with t = amount / 100, dc = (C - 0.09) / 0.05 and
# dl = (L - 0.75) / 0.15 for an OkLCh colour:
#
#     response = amp[b][kind][o] * gain[kind][o](t) * band[b](h) * exp(ac dc + al dl)
#     gain(t)  = g1 |t| + g2 t^2 + g3 |t|^3      (g4..g6 for positive amounts)
#     band(h)  = exp(-z^2 / 2),  z = wrap(h - centre) / e^(left or right)
#
# Each band's centre and widths are shared by its three sliders; each kind's gain curve and level dependence are
# shared by all bands.
#
# Calibration's primaries act on every colour, not on a band: each of its six sliders is a 3 x 3 matrix on linear
# ProPhoto RGB, I + A t + Q t^2 (Q for positive or negative amounts), every row of A and Q summing to 0 so greys stay
# grey. 18 constants per slider.

FIT_C, FIT_L = (0.09, 0.05), (0.75, 0.15)
CENTRES = (10, 50, 95, 140, 180, 255, 320, 350)
# starting signs and sizes per (kind, output): each kind's main output, then its smaller side effects
MIXER_START = {(0, 0): (-40, 40), (0, 1): (0.1, 0.1), (0, 2): (0.01, 0.01), (1, 0): (1, -1), (1, 1): (-0.6, 0.6),
               (1, 2): (0.01, -0.01), (2, 0): (1, -1), (2, 1): (0.1, -0.1), (2, 2): (-0.1, 0.1)}

def unpack(p, nb=8, nk=3):
    """(bands nb x [centre, left, right], kinds nk x 3 x [g1..g6, ac, al], amps nb x nk x 3)."""
    k = 3 * nb + 24 * nk
    return p[:3 * nb].reshape(nb, 3), p[3 * nb:k].reshape(nk, 3, 8), p[k:k + 3 * nb * nk].reshape(nb, nk, 3)


def model(p, t, dc, dl, nb=8, nk=3):
    """The responses (slider, output, amount, level, hue) at amounts `t` and level coordinates `dc`, `dl`."""
    bands, kinds, amps = unpack(p, nb, nk)
    hue = np.arange(HUES) * (360 / HUES)
    at, neg = np.abs(t), t < 0
    out = np.zeros((nb * nk, 3, len(t), len(dc), HUES))
    for b in range(nb):
        centre, left, right = bands[b]
        d = (hue - centre + 180) % 360 - 180
        shape = np.exp(-0.5 * (d / np.exp(np.where(d < 0, left, right))) ** 2)
        for k in range(nk):
            for o in range(3):
                g = kinds[k, o]
                gain = np.where(neg, g[0] * at + g[1] * at ** 2 + g[2] * at ** 3, g[3] * at + g[4] * at ** 2 + g[5] * at ** 3)
                level = np.exp(g[6] * dc + g[7] * dl)
                out[b * nk + k, o] = amps[b, k, o] * gain[:, None, None] * level[None, :, None] * shape[None, None, :]
    return out


def calibration_matrix(p, t):
    """One calibration slider's matrix at amount `t` (-1..1) from its 18 constants [A, Q positive, Q negative]."""
    rows = lambda v: np.c_[np.reshape(v, (3, 2)), -np.reshape(v, (3, 2)).sum(1)]
    return np.eye(3) + rows(p[:6]) * t + rows(p[6:12] if t > 0 else p[12:18]) * t * t


def fit_calibration(neutral, edited):
    """Fit one calibration slider's matrix to linear ProPhoto patches: `neutral` (n x 3) and `edited` {amount: n x 3};
    returns the constants and the mean OkLab x 100 error and effect."""
    from scipy.optimize import least_squares
    to_lab = lambda p: (M2 @ np.cbrt(np.clip(M1 @ (p @ np.linalg.inv(XYZ_TO_PROPHOTO).T).T, 0, None))).T
    lab0 = to_lab(neutral)
    target = {a: to_lab(e) for a, e in edited.items()}
    residual = lambda p: np.concatenate([((to_lab(np.clip(neutral @ calibration_matrix(p, a / 100).T, 0, None)) - target[a]) * 100).ravel()
                                         for a in edited])
    p = least_squares(residual, np.zeros(18)).x
    e = residual(p).reshape(len(edited), -1, 3)
    effect = np.mean([np.linalg.norm((target[a] - lab0) * 100, axis=1).mean() for a in edited])
    return p, float(np.linalg.norm(e, axis=2).mean()), float(effect)


def fit_model(measured, chroma, dc, dl, centres=CENTRES, nk=3, start=MIXER_START):
    """Fit the model to the measured responses (slider, amount, level, output, hue); returns the constants."""
    from scipy.optimize import least_squares
    t = np.array(AMOUNTS) / 100

    def oklab(v, o):
        """A response in OkLab x 100 at each level's chroma: hue shift and chroma change scale with chroma."""
        return (np.radians(v) * chroma[None, :, None] if o == 0 else v * chroma[None, :, None] if o == 1 else v) * 100

    nb, n = len(centres), len(centres) * nk
    target = np.stack([np.stack([oklab(measured[s, :, :, o], o) for o in range(3)]) for s in range(n)])

    def residual(p):
        m = model(p, t, dc, dl, nb, nk)
        return (np.stack([np.stack([oklab(m[s, o], o) for o in range(3)]) for s in range(n)]) - target).ravel()

    kinds = np.zeros((nk, 3, 8))
    for (k, o), (neg, pos) in start.items():
        kinds[k, o, 0], kinds[k, o, 3] = neg, pos
    p0 = np.r_[np.ravel([[c, 3.0, 3.0] for c in centres]), kinds.ravel(), np.ones(3 * n)]
    return least_squares(residual, p0, max_nfev=3000).x, residual


def fit(render_dir, out_rs):
    """Fit the mixer's and calibration's models to the Camera Raw renders in `render_dir` and write their constants to
    `out_rs`."""
    patches = [p for p in json.load(open(os.path.join(render_dir, "manifest.json")))["patches"] if p["ok"]]
    tif = lambda n: os.path.join(render_dir, f"mixer__{n}.tif")
    lab0 = oklab_patches(tif("neutral"), patches)
    light = np.array([p["L"] for p in patches])
    chroma = np.array([p["C"] for p in patches])
    c0 = np.hypot(lab0[:, 1], lab0[:, 2])
    # each level's chroma and lightness on Camera Raw's neutral render (it desaturates bright colours), as the fit's
    # coordinates of the measured rows
    level_c, level_l = [], []
    for v in LIGHTNESS:
        for c in CHROMA:
            m = (light == v) & (chroma == c)
            level_c.append(float(c0[m].mean() if m.sum() else c0[chroma == c].mean()))
            level_l.append(float(lab0[light == v, 0].mean()))
    level_c, level_l = np.array(level_c), np.array(level_l)
    dc, dl = (level_c - FIT_C[0]) / FIT_C[1], (level_l - FIT_L[0]) / FIT_L[1]
    every = [n for n, _ in sliders()]
    measure = lambda stems: np.stack([np.stack([response(lab0, oklab_patches(tif(f"{stem}{a:+d}"), patches), light, chroma)
                                                for a in AMOUNTS]) for stem in stems])
    params, residual = fit_model(measure(every[:24]), level_c, dc, dl)
    e = residual(params).reshape(24, 3, -1)
    for i, stem in enumerate(every[:24]):
        print(f"{stem:22s} rms " + " ".join(f"{np.sqrt((e[i, o] ** 2).mean()):.3f}" for o in range(3)) + " (hue, chroma, lightness; OkLab x 100)")
    pro = lambda n: (rgb_patches(tif(n), patches) ** (563 / 256)) @ ADOBE_TO_XYZ.T @ XYZ_TO_PROPHOTO.T
    neutral = pro("neutral")
    cal = []
    for stem in every[24:]:
        p, err, effect = fit_calibration(neutral, {a: pro(f"{stem}{a:+d}") for a in AMOUNTS})
        cal.append(p)
        print(f"{stem:22s} mean error {err:.3f} of an effect of {effect:.3f} (OkLab x 100)")
    bands, kinds, amps = unpack(params)
    num = lambda v: f"{v:.6e}"
    row = lambda vs: "[" + ", ".join(num(v) for v in vs) + "]"
    s = ["// Written by `tools/lr_mixer_charts.py fit`: LightCraft's model of the colour mixer, fitted to the tool's OkLCh",
         "// charts (provenance in docs/mixer-stage.md). Constants of the model only.",
         "",
         "/// The model's reference chroma and lightness: `dc = (C - centre) / scale`, `dl` likewise.",
         f"pub const REF_CHROMA: (f64, f64) = ({FIT_C[0]}, {FIT_C[1]});",
         f"pub const REF_LIGHT: (f64, f64) = ({FIT_L[0]}, {FIT_L[1]});",
         "",
         "/// Per band (Red, Orange, Yellow, Green, Aqua, Blue, Purple, Magenta): centre (OkLab hue, degrees), and the log",
         "/// width below and above it.",
         "pub const BANDS: [[f64; 3]; 8] = [" + ", ".join(row(b) for b in bands) + "];",
         "/// Per kind of slider (Hue, Saturation, Luminance) and output (hue shift, chroma change, lightness change):",
         "/// `[g1, g2, g3, g4, g5, g6, ac, al]`, the gain over the amount and the dependence on chroma and lightness.",
         "pub const KINDS: [[[f64; 8]; 3]; 3] = [",
         *[f"    [{', '.join(row(o) for o in k)}]," for k in kinds],
         "];",
         "/// Per band, kind and output: the amplitude.",
         "pub const AMPS: [[[f64; 3]; 3]; 8] = [",
         *[f"    [{', '.join(row(k) for k in a)}]," for a in amps],
         "];",
         "",
         "/// Calibration's sliders (Red Hue, Red Saturation, Green Hue, Green Saturation, Blue Hue, Blue Saturation): the",
         "/// first two columns of each row of `A`, `Q` positive and `Q` negative, row by row (the third makes the row sum 0).",
         "pub const CALIBRATION: [[f64; 18]; 6] = [",
         *[f"    {row(p)}," for p in cal],
         "];",
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
            write_dng(os.path.join(out, f"mixer__{name}.dng"), img, xmp(**settings))
    elif len(sys.argv) == 4 and sys.argv[1] == "fit":
        fit(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
