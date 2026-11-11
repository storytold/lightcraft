#!/usr/bin/env python3
"""Measure Lightroom's Whites and parametric curve on synthetic grey steps (process V9, docs/curve-stage.md).

    python3 tools/lr_tone_charts.py charts OUT_DIR
        Writes linear DNG charts (48 grey steps plus colour patches) with the settings embedded as XMP:
        one neutral chart, Whites at +-25/50/100 and each parametric region at +-25/50/75/100 (default splits).
    (render every DNG through Camera Raw to a 16-bit Adobe RGB (1998) TIFF with the same file name)
    python3 tools/lr_tone_charts.py tables RENDER_DIR OUT_RS
        Reads the renders and writes the Rust tables of `lightcraft_pipeline::curve_stage`.
    python3 tools/lr_tone_charts.py base RENDER_DIR OUT_RS
        Reads the neutral render and writes Lightroom's default base curve, the table of
        `lightcraft_pipeline::default_tone` (docs/default-tone.md).

The charts are original test data generated here; nothing in them comes from Adobe. Only Camera Raw's output is
measured. Requires numpy and tifffile (tools/requirements-lr-tone-charts.txt).
"""
import json
import os
import struct
import sys

import numpy as np

W, H = 2400, 1600
CELL = 60
# linear sRGB -> XYZ (D65) inverse, stored as the DNG's ColorMatrix1 so the chart decodes as scene-linear sRGB
XYZ_TO_SRGB = [[3.2404542, -1.5371385, -0.4985314], [-0.969266, 1.8760108, 0.041556], [0.0556434, -0.2040259, 1.0572252]]
WHITES = (-100, -50, -25, 25, 50, 100)
REGIONS = ("Shadows", "Darks", "Lights", "Highlights")
REGION_AMOUNTS = (-100, -75, -50, -25, 25, 50, 75, 100)


def xmp(**settings):
    """Camera Raw settings with everything else neutral (no sharpening, noise reduction, lens or vignette correction)."""
    d = dict(CameraProfile="Embedded", WhiteBalance="As Shot", Exposure2012="0.00", Contrast2012="0", Highlights2012="0",
             Shadows2012="0", Whites2012="0", Blacks2012="0", ToneCurveName2012="Linear", Sharpness="0", LuminanceSmoothing="0",
             ColorNoiseReduction="0", LensProfileEnable="0", PostCropVignetteAmount="0", VignetteAmount="0", Texture="0",
             Clarity2012="0", Dehaze="0", Vibrance="0", Saturation="0")
    d.update({k: str(v) for k, v in settings.items()})
    attrs = " ".join(f'crs:{k}="{v}"' for k, v in d.items())
    return ('<?xpacket begin="﻿" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/">'
            '<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" '
            'xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Version="16.5" crs:ProcessVersion="11.0" '
            f'crs:HasSettings="True" {attrs}/></rdf:RDF></x:xmpmeta><?xpacket end="w"?>').encode("utf8")


def write_dng(path, img, xm):
    """A minimal linear (LinearRaw) DNG: `img` is scene-linear RGB 0..1, height x width x 3."""
    h, w = img.shape[:2]
    px = (np.clip(img, 0, 1) * 65535 + 0.5).astype("<u2").tobytes()
    make, model = b"LCTEST\0", b"LCTEST Synthetic\0"
    cm = b"".join(struct.pack("<2i", int(round(v * 10000)), 10000) for row in XYZ_TO_SRGB for v in row)
    entries = [
        (254, 4, 1, struct.pack("<I", 0)), (256, 4, 1, struct.pack("<I", w)), (257, 4, 1, struct.pack("<I", h)),
        (258, 3, 3, struct.pack("<3H", 16, 16, 16)), (259, 3, 1, struct.pack("<H", 1)), (262, 3, 1, struct.pack("<H", 34892)),
        (271, 2, len(make), make), (272, 2, len(model), model), (273, 4, 1, None), (274, 3, 1, struct.pack("<H", 1)),
        (277, 3, 1, struct.pack("<H", 3)), (278, 4, 1, struct.pack("<I", h)), (279, 4, 1, struct.pack("<I", len(px))),
        (284, 3, 1, struct.pack("<H", 1)), (700, 1, len(xm), xm), (50706, 1, 4, bytes([1, 4, 0, 0])),
        (50707, 1, 4, bytes([1, 1, 0, 0])), (50708, 2, len(model), model), (50714, 3, 3, struct.pack("<3H", 0, 0, 0)),
        (50717, 3, 3, struct.pack("<3H", 65535, 65535, 65535)), (50721, 10, 9, cm),
        (50728, 5, 3, b"".join(struct.pack("<2I", 1, 1) for _ in range(3))), (50730, 10, 1, struct.pack("<2i", 0, 1)),
        (50778, 3, 1, struct.pack("<H", 21)),
    ]
    ifd = 8
    off = ifd + 2 + 12 * len(entries) + 4
    blob, rows = b"", []
    for tag, typ, cnt, val in entries:
        if val is None:
            rows.append((tag, typ, cnt, None))
        elif len(val) <= 4:
            rows.append((tag, typ, cnt, val.ljust(4, b"\0")))
        else:
            rows.append((tag, typ, cnt, struct.pack("<I", off + len(blob))))
            blob += val + (b"\0" if len(val) % 2 else b"")
    strip = off + len(blob)
    out = bytearray(b"II*\0" + struct.pack("<I", ifd) + struct.pack("<H", len(entries)))
    for tag, typ, cnt, v in rows:
        out += struct.pack("<HHI", tag, typ, cnt) + (v if v is not None else struct.pack("<I", strip))
    out += struct.pack("<I", 0) + blob + px
    with open(path, "wb") as f:
        f.write(out)


def chart():
    """48 grey steps (geometric, 0.002 to 1) then OkLCh colour patches; patches outside sRGB are left out."""
    m2i = np.linalg.inv(np.array([[0.2104542553, 0.793617785, -0.0040720468], [1.9779984951, -2.428592205, 0.4505937099],
                                  [0.0259040371, 0.7827717662, -0.808675766]]))
    m1i = np.linalg.inv(np.array([[0.8189330101, 0.3618667424, -0.1288597137], [0.0329845436, 0.9293118715, 0.0361456387],
                                  [0.0482003018, 0.2643662691, 0.6338517070]]))
    # XYZ -> linear sRGB as the exact inverse of the sRGB primaries' matrix (the patches' colours)
    x2s = np.linalg.inv(np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]]))
    img = np.full((H, W, 3), 0.18)
    patches = []

    def put(c, **k):
        """Place patch colour `c` in the next cell; a colour outside 0..1 is listed but left black."""
        i = len(patches)
        x, y = (i % (W // CELL)) * CELL, (i // (W // CELL)) * CELL
        ok = bool(np.all(c >= 0) and np.all(c <= 1))
        if ok:
            img[y + 6:y + CELL - 6, x + 6:x + CELL - 6] = c
        patches.append(dict(y=y + 6, x=x + 6, w=CELL - 12, h=CELL - 12, ok=ok, **k))

    for g in np.geomspace(0.002, 1, 48):
        put(np.array([g, g, g]), kind="grey", v=float(g))
    for lightness in (0.25, 0.4, 0.55, 0.7, 0.82, 0.92):
        for chroma in (0.04, 0.08, 0.12):
            for hue in range(0, 360, 15):
                lab = np.array([lightness, chroma * np.cos(np.radians(hue)), chroma * np.sin(np.radians(hue))])
                put(x2s @ (m1i @ ((m2i @ lab) ** 3)), kind="colour", L=lightness, C=chroma, hue=hue)
    return img, patches


def variants():
    """Name and XMP settings of every chart: neutral, each Whites amount, each region amount."""
    v = {"neutral": {}}
    for a in WHITES:
        v[f"whites{a:+d}"] = {"Whites2012": a}
    for r in REGIONS:
        for a in REGION_AMOUNTS:
            v[f"par_{r.lower()}{a:+d}"] = {"Parametric" + r: a}
    return v


# ---- tables -------------------------------------------------------------------------------------------------

ADOBE_TO_XYZ = np.array([[0.5767309, 0.1855540, 0.1881852], [0.2973769, 0.6273491, 0.0752741], [0.0270343, 0.0706872, 0.9911085]])
# XYZ (D65) -> linear ProPhoto RGB (D50, Bradford), as `RgbSpace::to_space`
XYZ_TO_PROPHOTO = (np.array([[1.3459433, -0.2556075, -0.0511118], [-0.5445989, 1.5081673, 0.0205351], [0.0, 0.0, 1.2118128]])
                   @ np.array([[1.0478112, 0.0228866, -0.0501270], [0.0295424, 0.9904844, -0.0170491],
                               [-0.0092345, 0.0150436, 0.7521316]]))


def srgb_encode(v):
    """Linear to sRGB-encoded values."""
    v = np.clip(v, 0, None)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * v ** (1 / 2.4) - 0.055)


def grey_levels(path, patches):
    """The grey steps of a Camera Raw render as sRGB-encoded linear ProPhoto values (the curve stage's domain)."""
    import tifffile
    a = tifffile.imread(path).astype(np.float64)
    a /= 65535.0 if a.max() > 255 else 255.0
    out = []
    for q in patches:
        if q["kind"] != "grey" or not q["ok"]:
            continue
        rgb = a[q["y"] + 8:q["y"] + q["h"] - 8, q["x"] + 8:q["x"] + q["w"] - 8].reshape(-1, 3).mean(0)
        xyz = ADOBE_TO_XYZ @ (np.clip(rgb, 0, 1) ** (563 / 256))
        out.append(srgb_encode((XYZ_TO_PROPHOTO @ xyz)[1]))
    return np.array(out)


def delta_table(neutral, edited):
    """Output minus input over 257 evenly spaced inputs, from matched grey steps; zero at both ends."""
    o = np.argsort(neutral)
    x, y = neutral[o], np.maximum.accumulate(edited[o])
    grid = np.linspace(0, 1, 257)
    d = np.interp(grid, x, y - x)
    lo, hi = grid < x[0], grid > x[-1]
    d[lo] = np.interp(grid[lo], [0, x[0]], [0, d[~lo][0]])
    d[hi] = np.interp(grid[hi], [x[-1], 1], [d[~hi][-1], 0])
    return d


def tables(render_dir, out_rs):
    """Measure the Camera Raw renders in `render_dir` and write the Rust tables to `out_rs`."""
    patches = json.load(open(os.path.join(render_dir, "manifest.json")))["patches"]
    tif = lambda n: os.path.join(render_dir, f"curve__{n}.tif")
    n0 = grey_levels(tif("neutral"), patches)
    rows = lambda names: [delta_table(n0, grey_levels(tif(n), patches)) for n in names]
    whites = rows([f"whites{a:+d}" for a in WHITES])
    regions = [rows([f"par_{r.lower()}{a:+d}" for a in REGION_AMOUNTS]) for r in REGIONS]
    fmt = lambda t: "[" + ", ".join(f"{v:.6f}" for v in t) + "]"
    s = ["// @generated by tools/lr_tone_charts.py from Camera Raw renders of its synthetic grey steps (docs/curve-stage.md).",
         "// Output minus input of each setting on sRGB-encoded linear ProPhoto values, 257 evenly spaced inputs from 0 to 1.",
         "",
         f"pub const WHITES_AMOUNTS: [f32; {len(WHITES)}] = [{', '.join(f'{a}.0' for a in WHITES)}];",
         f"pub static WHITES: [[f32; 257]; {len(WHITES)}] = [", *[f"    {fmt(t)}," for t in whites], "];",
         "",
         "/// Shadows, Darks, Lights, Highlights at the default splits (25, 50, 75).",
         f"pub const REGION_AMOUNTS: [f32; {len(REGION_AMOUNTS)}] = [{', '.join(f'{a}.0' for a in REGION_AMOUNTS)}];",
         f"pub static REGIONS: [[[f32; 257]; {len(REGION_AMOUNTS)}]; 4] = ["]
    for r in regions:
        s += ["    ["] + [f"        {fmt(t)}," for t in r] + ["    ],"]
    s += ["];", ""]
    with open(out_rs, "w") as f:
        f.write("\n".join(s))


def base(render_dir, out_rs):
    """Write the neutral render's grey steps (chart value in, linear ProPhoto out) as the default base curve to `out_rs`."""
    patches = json.load(open(os.path.join(render_dir, "manifest.json")))["patches"]
    steps = [q["v"] for q in patches if q["kind"] == "grey" and q["ok"]]
    enc = grey_levels(os.path.join(render_dir, "curve__neutral.tif"), patches)
    lin = np.where(enc <= 0.04045, enc / 12.92, ((enc + 0.055) / 1.055) ** 2.4)
    lin = np.maximum.accumulate(np.clip(lin, 0, 1))
    s = ["// @generated by tools/lr_tone_charts.py from Camera Raw's render of its neutral synthetic chart (docs/default-tone.md).",
         "// Grey steps: the chart's linear value in, the render's linear ProPhoto value out.",
         "",
         f"pub const GREY: [[f32; 2]; {len(steps)}] = [",
         *[f"    [{x:.6f}, {y:.6f}]," for x, y in zip(steps, lin)],
         "];",
         ""]
    with open(out_rs, "w") as f:
        f.write("\n".join(s))


def main():
    """Command line: `charts OUT_DIR`, `tables RENDER_DIR OUT_RS` or `base RENDER_DIR OUT_RS`."""
    if len(sys.argv) == 3 and sys.argv[1] == "charts":
        out = sys.argv[2]
        os.makedirs(out, exist_ok=True)
        img, patches = chart()
        json.dump(dict(patches=patches), open(os.path.join(out, "manifest.json"), "w"))
        for name, settings in variants().items():
            write_dng(os.path.join(out, f"curve__{name}.dng"), img, xmp(**settings))
    elif len(sys.argv) == 4 and sys.argv[1] == "tables":
        tables(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 4 and sys.argv[1] == "base":
        base(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
