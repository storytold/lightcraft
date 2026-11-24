#!/usr/bin/env python3
"""Measure Lightroom's Whites and parametric curve on synthetic grey steps (process V9, docs/curve-stage.md).

    python3 tools/lr_tone_charts.py charts OUT_DIR
        Writes linear DNG charts (48 grey steps plus colour patches) with the settings embedded as XMP:
        one neutral chart, Whites at +-25/50/100 and each parametric region at +-25/50/75/100 (default splits).
    (render every DNG through Camera Raw to a 16-bit Adobe RGB (1998) TIFF with the same file name)
    python3 tools/lr_tone_charts.py fit RENDER_DIR OUT_RS
        Reads the renders, fits LightCraft's model of each setting to them and writes the model's constants
        (`lightcraft_pipeline::curve_stage_model`). The measurements themselves are not written out.
    python3 tools/lr_tone_charts.py base RENDER_DIR OUT_RS
        Reads the neutral render, fits the default base curve's knots to its grey steps and writes them
        (`lightcraft_pipeline::default_tone_model`, docs/default-tone.md). The grey steps are not written out.

The charts are original test data generated here; nothing in them comes from Adobe. Only Camera Raw's output is
measured. Requires numpy, scipy and tifffile (tools/requirements-lr-tone-charts.txt).
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


# ---- measuring ----------------------------------------------------------------------------------------------

ADOBE_TO_XYZ = np.array([[0.5767309, 0.1855540, 0.1881852], [0.2973769, 0.6273491, 0.0752741], [0.0270343, 0.0706872, 0.9911085]])
# XYZ (D65) -> linear ProPhoto RGB (D50, Bradford), as `RgbSpace::to_space`
XYZ_TO_PROPHOTO = (np.array([[1.3459433, -0.2556075, -0.0511118], [-0.5445989, 1.5081673, 0.0205351], [0.0, 0.0, 1.2118128]])
                   @ np.array([[1.0478112, 0.0228866, -0.0501270], [0.0295424, 0.9904844, -0.0170491],
                               [-0.0092345, 0.0150436, 0.7521316]]))


def srgb_encode(v):
    """Linear to sRGB-encoded values."""
    v = np.clip(v, 0, None)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * v ** (1 / 2.4) - 0.055)


ADOBE_RGB_NAME = "Adobe RGB (1998)"


def read_render(path):
    """A Camera Raw render as RGB 0..1, after checking it is the 16-bit Adobe RGB (1998) TIFF the fit assumes.

    Another export setting would still give plausible numbers, but wrong ones, so it is refused.
    """
    import tifffile
    with tifffile.TiffFile(path) as t:
        page = t.pages[0]
        icc = page.tags.get(34675)  # InterColorProfile
        profile = bytes(icc.value) if icc is not None else b""
        a = page.asarray()
    if a.dtype != np.uint16 or a.ndim != 3 or a.shape[2] != 3:
        raise SystemExit(f"{path}: expected a 16-bit RGB TIFF, got {a.dtype} {a.shape}")
    # the profile's description: ASCII in ICC v2, UTF-16 in v4
    if not any(n in profile for n in (ADOBE_RGB_NAME.encode(), ADOBE_RGB_NAME.encode("utf-16-be"))):
        raise SystemExit(f"{path}: expected an embedded {ADOBE_RGB_NAME} profile (export from Camera Raw in that colour space)")
    return a.astype(np.float64) / 65535.0


def grey_levels(path, patches):
    """The grey steps of a Camera Raw render as sRGB-encoded linear ProPhoto values (the curve stage's domain)."""
    a = read_render(path)
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


# ---- fit ----------------------------------------------------------------------------------------------------
#
# LightCraft's own model of each setting, fitted to the measured grey steps. Nothing measured is written out: only
# the model's constants. Per slider direction (negative, positive) the response (output minus input) is the sum of
# two bumps, each with a gain over the amount and a shape whose position and width change with the amount:
#
#     t = |amount| / 100
#     response(x) = sum over the two bumps of (a1 t + a2 t^2 + a3 t^3) * bump(x, e^(p0 + p1 t), e^(q0 + q1 t))
#     bump(x, p, q) = (x / m)^p ((1 - x) / (1 - m))^q with m = p / (p + q): 0 at black and white, 1 at its peak m
#
# The fit keeps every response growing with the amount at every input, and never acting against the slider.

FIT_SEED = 7
FIT_STARTS = 16
FIT_AMOUNTS = np.linspace(0, 1, 21)
GRID = np.linspace(0, 1, 257)


def bump(x, p, q):
    """A bump of peak 1 at p / (p + q), 0 at black and white."""
    m = p / (p + q)
    return np.exp(p * np.log(np.clip(x, 1e-12, 1) / m) + q * np.log(np.clip(1 - x, 1e-12, 1) / (1 - m)))


def response(params, t, x=GRID):
    """The model's response of one slider direction at amount `t` (0..1) for inputs `x`."""
    out = 0.0
    for a1, a2, a3, p0, p1, q0, q1 in np.reshape(params, (2, 7)):
        out = out + (a1 * t + a2 * t * t + a3 * t ** 3) * bump(x, np.exp(p0 + p1 * t), np.exp(q0 + q1 * t))
    return out


def fit_direction(amounts, deltas, sign):
    """Fit one slider direction to its measured deltas (257 inputs each); returns the constants and the error."""
    from scipy.optimize import least_squares
    rows = [(abs(a) / 100, d) for a, d in zip(amounts, deltas) if np.sign(a) == sign]

    def err(params):
        """Error of the rendered curve in 8-bit levels, both sides clamped to 0..1 as the stage clamps."""
        return np.concatenate([(np.clip(GRID + response(params, t), 0, 1) - np.clip(GRID + d, 0, 1)) * 255 for t, d in rows])

    def residual(params):
        grow = np.array([sign * response(params, t) for t in FIT_AMOUNTS])
        return np.concatenate([err(params), 30 * 255 * np.minimum(np.diff(grow, axis=0), 0).ravel(),
                               30 * 255 * np.minimum(grow, 0).ravel()])

    rng = np.random.default_rng(FIT_SEED)
    starts = [np.ravel([[sign * rng.uniform(0, 0.3), 0, 0, rng.uniform(-1.5, 2.5), 0, rng.uniform(-1.5, 2.5), 0]
                        for _ in range(2)]) for _ in range(FIT_STARTS)]
    best = min((least_squares(residual, s, max_nfev=3000) for s in starts), key=lambda r: r.cost)
    e = err(best.x)
    return best.x, float(np.sqrt((e ** 2).mean())), float(np.abs(e).max())


def fit(render_dir, out_rs):
    """Fit the model to the Camera Raw renders in `render_dir` and write its constants to `out_rs`."""
    patches = json.load(open(os.path.join(render_dir, "manifest.json")))["patches"]
    tif = lambda n: os.path.join(render_dir, f"curve__{n}.tif")
    n0 = grey_levels(tif("neutral"), patches)
    rows = lambda names: [delta_table(n0, grey_levels(tif(n), patches)) for n in names]
    sets = [("Whites", WHITES, rows([f"whites{a:+d}" for a in WHITES]))]
    sets += [(r, REGION_AMOUNTS, rows([f"par_{r.lower()}{a:+d}" for a in REGION_AMOUNTS])) for r in REGIONS]
    from concurrent.futures import ProcessPoolExecutor
    jobs = [(name, sign, amounts, deltas) for name, amounts, deltas in sets for sign in (-1, 1)]
    with ProcessPoolExecutor() as pool:
        done = list(pool.map(fit_direction, *zip(*[(a, d, s) for _, s, a, d in jobs])))
    fitted = {}
    for (name, sign, _, _), (params, rms, worst) in zip(jobs, done):
        fitted[name, sign] = params
        print(f"{name:10s} {'+' if sign > 0 else '-'}  rms {rms:.2f}  max {worst:.2f} (8-bit levels)")
    num = lambda v: f"{v:.6e}"
    resp = lambda name, sign: "[" + ", ".join("[" + ", ".join(num(v) for v in b) + "]" for b in np.reshape(fitted[name, sign], (2, 7))) + "]"
    pair = lambda name: f"[{resp(name, -1)}, {resp(name, 1)}]"
    s = ["// Written by `tools/lr_tone_charts.py fit`: LightCraft's model of Whites and the parametric regions, fitted to the",
         "// tool's grey-step charts (provenance in docs/curve-stage.md). Constants of the model only.",
         "",
         "/// One bump of a response: `[a1, a2, a3, p0, p1, q0, q1]`, gain `a1 t + a2 t² + a3 t³` (t = |amount| / 100), shape",
         "/// exponents `e^(p0 + p1 t)` and `e^(q0 + q1 t)` ([`crate::curve_stage`]).",
         "pub type Bump = [f64; 7];",
         "/// A slider direction's response: two bumps, added.",
         "pub type Response = [Bump; 2];",
         "",
         "/// Whites: negative, positive.",
         f"pub const WHITES: [Response; 2] = {pair('Whites')};",
         "",
         "/// Shadows, Darks, Lights, Highlights at the default splits (25, 50, 75): negative, positive.",
         "pub const REGIONS: [[Response; 2]; 4] = [",
         *[f"    {pair(r)}," for r in REGIONS],
         "];",
         ""]
    with open(out_rs, "w") as f:
        f.write("\n".join(s))


BASE_KNOTS = 12


def monotone(xs, ys):
    """`lightcraft_color::spline::MonotoneCurve`: the same tangents and Fritsch-Carlson limiter, as a function."""
    n = len(xs)
    d = np.diff(ys) / np.diff(xs)
    ms = np.zeros(n)
    ms[0], ms[-1] = d[0], d[-1]
    for i in range(1, n - 1):
        ms[i] = 0 if d[i - 1] * d[i] <= 0 else (d[i - 1] + d[i]) / 2
    for i in range(n - 1):
        if d[i] == 0:
            ms[i] = ms[i + 1] = 0
            continue
        a, b = ms[i] / d[i], ms[i + 1] / d[i]
        if a * a + b * b > 9:
            t = 3 / np.sqrt(a * a + b * b)
            ms[i], ms[i + 1] = t * a * d[i], t * b * d[i]

    def at(q):
        q = np.asarray(q, float)
        i = np.clip(np.searchsorted(xs, q) - 1, 0, n - 2)
        h = xs[i + 1] - xs[i]
        t = (q - xs[i]) / h
        v = ((2 * t ** 3 - 3 * t ** 2 + 1) * ys[i] + (t ** 3 - 2 * t ** 2 + t) * h * ms[i] + (-2 * t ** 3 + 3 * t ** 2) * ys[i + 1]
             + (t ** 3 - t ** 2) * h * ms[i + 1])
        return np.where(q <= xs[0], ys[0], np.where(q >= xs[-1], ys[-1], v))
    return at


def base(render_dir, out_rs):
    """Fit the default base curve's knots to the neutral render's grey steps (chart value in, linear ProPhoto out) and
    write them to `out_rs`: 12 knots evenly spaced in stops from the darkest step (0) to white (1)."""
    from scipy.optimize import least_squares
    patches = json.load(open(os.path.join(render_dir, "manifest.json")))["patches"]
    steps = np.array([q["v"] for q in patches if q["kind"] == "grey" and q["ok"]])
    enc = grey_levels(os.path.join(render_dir, "curve__neutral.tif"), patches)
    lin = np.where(enc <= 0.04045, enc / 12.92, ((enc + 0.055) / 1.055) ** 2.4)
    lin = np.maximum.accumulate(np.clip(lin, 0, 1))
    stops = np.log2(steps)
    kx = np.linspace(stops.min(), 0.0, BASE_KNOTS)

    def knots(p):
        """Increasing knots from 0 at the darkest step to exactly 1 at white."""
        w = np.exp(p)
        return np.concatenate([[0.0], np.cumsum(w) / w.sum()])

    err = lambda p: (srgb_encode(monotone(kx, knots(p))(stops)) - srgb_encode(lin)) * 255
    p0 = np.log(np.maximum(np.diff(np.interp(kx, stops, lin)), 1e-5))
    ky = knots(least_squares(err, p0, max_nfev=20000).x)
    e = (srgb_encode(monotone(kx, ky)(stops)) - srgb_encode(lin)) * 255
    print(f"base curve: rms {np.sqrt((e ** 2).mean()):.2f}  max {np.abs(e).max():.2f} (8-bit levels)")
    row = lambda vs: "[" + ", ".join(f"{v:.6e}" for v in vs) + "]"
    s = ["// Written by `tools/lr_tone_charts.py base`: LightCraft's default base curve, fitted to the tool's neutral",
         "// grey-step chart (provenance in docs/default-tone.md). Knots of the curve only.",
         "",
         "/// The knots' range in stops of the chart's linear value: from the darkest grey step to white.",
         f"pub const STOPS: (f64, f64) = ({kx[0]:.6e}, 0.0);",
         "/// The curve's linear ProPhoto output at the knots, evenly spaced over [`STOPS`].",
         f"pub const KNOTS: [f64; {BASE_KNOTS}] = {row(ky)};",
         ""]
    with open(out_rs, "w") as f:
        f.write("\n".join(s))


def main():
    """Command line: `charts OUT_DIR`, `fit RENDER_DIR OUT_RS` or `base RENDER_DIR OUT_RS`."""
    if len(sys.argv) == 3 and sys.argv[1] == "charts":
        out = sys.argv[2]
        os.makedirs(out, exist_ok=True)
        img, patches = chart()
        json.dump(dict(patches=patches), open(os.path.join(out, "manifest.json"), "w"))
        for name, settings in variants().items():
            write_dng(os.path.join(out, f"curve__{name}.dng"), img, xmp(**settings))
    elif len(sys.argv) == 4 and sys.argv[1] == "fit":
        fit(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 4 and sys.argv[1] == "base":
        base(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
