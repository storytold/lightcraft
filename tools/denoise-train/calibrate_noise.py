"""Measure a camera's raw noise per ISO from ordinary photos, as numbers only.

Offline training tooling for AI Denoise; not part of LightCraft.

`rawdump noise` measures 8 x 8 blocks of each Bayer plane (mean, variance of a high-pass residual) and reports, per
brightness bin, low percentiles of the variance. Flat areas give the lowest variances, so the 10th percentile,
corrected for the spread of a 64-sample variance estimate, follows the sensor's noise curve. Per ISO we fit the
Poisson-Gaussian model `variance = a * mean + b` (white = 1). No picture data is stored: the output is the fitted
`a`, `b` per ISO and camera. `train.py --noise-cal <this file>` trains on it: part of each batch gets a measured
camera's noise exactly, and the read-noise relation for the rest is fitted to these numbers.

    python calibrate_noise.py --rawdump <exe> --out noise-<camera>.json --sample 200 <folder> [<folder> ...]

The folders are only read.
"""

import argparse
import json
import math
import os
import random
import subprocess
import sys
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor

EXT = (".cr3", ".cr2", ".arw", ".nef", ".dng", ".raf", ".orf", ".rw2")
# 10th percentile of a chi-square variance estimate with 63 degrees of freedom, relative to the true variance
P10_BIAS = 0.772


def files_in(folders):
    out = []
    for d in folders:
        for root, _dirs, names in os.walk(d):
            out += [os.path.join(root, n) for n in names if n.lower().endswith(EXT)]
    return out


def fit(points):
    """Weighted least squares of var = a*m + b, with relative weights (each point counts by its relative error).

    Read noise can't be negative: when the free fit puts `b` below zero (read noise too small for these photos to
    resolve), `a` is refitted with `b = 0`, so the file never holds a variance that is negative in the shadows."""
    import numpy as np

    m = np.array([p[0] for p in points])
    v = np.array([p[1] for p in points])
    w = 1.0 / np.maximum(v, 1e-12)
    A = np.stack([m, np.ones_like(m)], 1) * w[:, None]
    (a, b), *_ = np.linalg.lstsq(A, v * w, rcond=None)
    if b < 0:
        a, b = float(np.sum(m * v * w * w) / np.sum(m * m * w * w)), 0.0
    return float(a), float(b)


def build_calibration(measurements):
    """`rawdump noise` outputs (dicts; None for a skipped file) -> {camera: {iso: {"photos": n, plane: {a, b}}}}.

    A plane whose fit gives no positive, finite shot-noise slope is left out (with a message), so every number
    written is one `train.py` can use."""
    # per (camera, iso, plane): mean bin -> list of p10 variances (one per photo)
    pts = defaultdict(lambda: defaultdict(list))
    counts = defaultdict(int)
    for d in measurements:
        if not d or not d.get("iso"):
            continue
        cam = f"{d['make']} {d['model']}".strip()
        counts[(cam, d["iso"])] += 1
        for pl in d["planes"]:
            for b in pl["bins"]:
                if b["n"] >= 50:
                    pts[(cam, d["iso"], pl["plane"])][round(b["mean"], 6)].append(b["p10"] / P10_BIAS)

    result = defaultdict(dict)
    for (cam, iso, plane), bins in sorted(pts.items()):
        # the lowest photo's value per bin (the flattest content), above the darkest bins where quantisation rules
        points = [(m, min(v)) for m, v in bins.items() if 0.002 <= m <= 0.6]
        if len(points) < 5:
            continue
        aa, bb = fit(points)
        if not (math.isfinite(aa) and math.isfinite(bb) and aa > 0):
            print(f"{cam} ISO {iso} {plane}: no usable fit (a={aa}, b={bb}); left out", file=sys.stderr)
            continue
        result[cam].setdefault(str(iso), {"photos": counts[(cam, iso)]})[plane] = {"a": aa, "b": bb}
    return dict(result)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rawdump", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--sample", type=int, default=200)
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("folders", nargs="+")
    a = ap.parse_args()
    files = files_in(a.folders)
    random.Random(1).shuffle(files)
    files = files[: a.sample]
    print(f"measuring {len(files)} files", flush=True)

    def one(f):
        # (a file that hangs, a helper that can't start or output that isn't JSON skips that file, not the batch)
        try:
            p = subprocess.run([a.rawdump, "noise", f], capture_output=True, text=True, encoding="utf-8", errors="replace",
                               timeout=600)
            if p.returncode != 0:
                print(f"skipped {f}: rawdump exit {p.returncode}: {p.stderr.strip()}", file=sys.stderr, flush=True)
                return None
            return json.loads(p.stdout)
        except (OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as e:
            print(f"skipped {f}: {e}", file=sys.stderr, flush=True)
            return None

    with ThreadPoolExecutor(a.jobs) as ex:
        measurements = []
        for i, d in enumerate(ex.map(one, files)):
            measurements.append(d)
            if i % 20 == 0:
                print(f"{i} done", flush=True)
    result = build_calibration(measurements)
    if not result:
        # (a mistyped folder, or raws the helper can't read): keep any earlier calibration rather than empty it
        print(f"no noise could be fitted from {len(files)} files; {a.out} is left as it was", file=sys.stderr)
        return 1
    out = {"model": "variance = a * mean + b, white = 1, black subtracted, per Bayer plane",
           "cameras": result}
    # the documented `<scratch>/noise/camera.json` needs its folder: made here, not after the photos are measured and lost
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    json.dump(out, open(a.out, "w"), indent=1)
    for cam, isos in result.items():
        for iso, v in sorted(isos.items(), key=lambda kv: int(kv[0])):
            g = v.get("G1", {})
            print(f"{cam} ISO {iso} ({v['photos']} photos): a={g.get('a', 0):.3e} b={g.get('b', 0):.3e}")


if __name__ == "__main__":
    sys.exit(main())
