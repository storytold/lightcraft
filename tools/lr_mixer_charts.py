#!/usr/bin/env python3
"""Measure Lightroom's colour mixer on synthetic colour charts (process V9, docs/mixer-stage.md).

    python3 tools/lr_mixer_charts.py charts OUT_DIR
        Writes linear DNG charts (OkLCh patches: 72 hues x 3 chroma x 3 lightness levels) with the settings embedded
        as XMP: one neutral chart and every mixer band's Hue, Saturation and Luminance and Calibration's red, green
        and blue Hue and Saturation at -100, -75, -50, -25, 25, 50, 75 and 100.
    (render every DNG through Camera Raw to a 16-bit Adobe RGB (1998) TIFF with the same file name)
    python3 tools/lr_mixer_charts.py table RENDER_DIR OUT_BIN
        Reads the renders and writes the response table of `lightcraft_pipeline::mixer_stage` (format below).

The charts are original test data generated here; nothing in them comes from Adobe. Only Camera Raw's output is
measured. Requires numpy and tifffile (tools/requirements-lr-tone-charts.txt).

Table format, little-endian: b"LCCS", u32 version 1, u32 sliders, amounts, rows per amount (9), outputs (3), hue
bins (72); f32 amounts; f32 chroma levels (3 per lightness level, 9); f32 lightness levels (3). Then for each slider, amount, lightness
level, chroma level and output: f32 scale and 72 i8 (value = scale x i8). Outputs per OkLab hue bin (5 degrees):
hue shift in degrees, relative chroma change, lightness change.
"""
import json
import os
import struct
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lr_tone_charts import ADOBE_TO_XYZ, H, W, write_dng, xmp  # noqa: E402

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


def table(render_dir, out_bin):
    """Measure the Camera Raw renders in `render_dir` and write the binary table to `out_bin`."""
    patches = [p for p in json.load(open(os.path.join(render_dir, "manifest.json")))["patches"] if p["ok"]]
    tif = lambda n: os.path.join(render_dir, f"mixer__{n}.tif")
    lab0 = oklab_patches(tif("neutral"), patches)
    light = np.array([p["L"] for p in patches])
    chroma = np.array([p["C"] for p in patches])
    c0 = np.hypot(lab0[:, 1], lab0[:, 2])
    # Camera Raw's neutral render desaturates bright colours, so each lightness level has its own chroma keys
    chroma_levels = []
    for v in LIGHTNESS:
        for c in CHROMA:
            m = (light == v) & (chroma == c)
            chroma_levels.append(float(c0[m].mean() if m.sum() else c0[chroma == c].mean()))
    light_levels = [float(lab0[light == v, 0].mean()) for v in LIGHTNESS]
    names = sliders()
    out = bytearray(b"LCCS" + struct.pack("<6I", 1, len(names), len(AMOUNTS), 9, 3, HUES))
    out += struct.pack(f"<{len(AMOUNTS)}f", *AMOUNTS) + struct.pack("<9f", *chroma_levels) + struct.pack("<3f", *light_levels)
    for stem, _ in names:
        for a in AMOUNTS:
            t = response(lab0, oklab_patches(tif(f"{stem}{a:+d}"), patches), light, chroma)
            for row in t.reshape(-1, HUES):
                scale = float(np.abs(row).max()) / 127.0
                q = np.zeros(HUES) if scale == 0 else np.round(row / scale)
                out += struct.pack("<f", scale) + q.astype(np.int8).tobytes()
    with open(out_bin, "wb") as f:
        f.write(out)


def main():
    """Command line: `charts OUT_DIR` or `table RENDER_DIR OUT_BIN`."""
    if len(sys.argv) == 3 and sys.argv[1] == "charts":
        out = sys.argv[2]
        os.makedirs(out, exist_ok=True)
        img, patches = chart()
        json.dump(dict(patches=patches), open(os.path.join(out, "manifest.json"), "w"))
        for name, settings in variants().items():
            write_dng(os.path.join(out, f"mixer__{name}.dng"), img, xmp(**settings))
    elif len(sys.argv) == 4 and sys.argv[1] == "table":
        table(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
