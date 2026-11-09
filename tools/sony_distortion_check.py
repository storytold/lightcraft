#!/usr/bin/env python3
"""Compare LightCraft's Sony embedded distortion correction with the camera's own JPEG.

    python3 -m venv .venv-distortion && .venv-distortion/bin/pip install -r tools/requirements-sony-distortion-check.txt
    .venv-distortion/bin/python tools/sony_distortion_check.py --cli target/release/lightcraft-cli \
        --work WORKDIR [--json results.json] PHOTO_DIR [PHOTO_DIR…]

Every ARW with a JPG of the same name is rendered by LightCraft twice, embedded correction on
and off, and measured against the camera JPEG (shot with Lens Comp. Distortion on Auto):

* agreement: offsets of matching features after the best similarity (scale, rotation, shift)
  between the render and the JPEG, by distance from the centre (0 = centre, 1 = corner);
* straightness: how far each checkerboard row and column bows away from a straight line.

Checkerboard shots use the board's corners (sub-pixel), other shots SIFT features (agreement
only). Offsets are in pixels of the camera JPEG (6000 px wide on the A7 III); the "@2376"
columns rescale them to the 2376 px exports docs/sony-lens-corrections.md reports (method and results
there, under "ILCE-7M3 validation").
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys

import cv2
import numpy as np
from scipy.spatial import cKDTree

WORK_LONG_EDGE = 3000  # detection resolution; coordinates are rescaled to the JPEG's pixels
BINS = [0.0, 0.25, 0.5, 0.75, 0.9, 1.01]
REPORT_WIDTH = 2376


def render(cli, arw, out, on):
    """Render `arw` with the profile correction `on` or off to `out`, reusing an output newer than the CLI and ARW."""
    settings = out + ".settings.json"
    with open(settings, "w") as f:
        json.dump({"optics": {"lens_profile": on, "profile_distortion": 100}}, f)
    exe = shutil.which(cli) or cli
    current = os.path.exists(out) and os.path.getmtime(out) >= max(os.path.getmtime(exe), os.path.getmtime(arw))
    if not current:
        r = subprocess.run([exe, "render", arw, "-o", out, "--settings", settings], capture_output=True)
        if r.returncode != 0:
            err = r.stderr.decode(errors="replace").strip()
            raise RuntimeError(f"render failed ({r.returncode}) for {arw}: {err[-500:]}")
    return out


def load_gray(path):
    """Grey image at the working resolution and the factor back to its own full-size pixels."""
    img = cv2.imread(path, cv2.IMREAD_GRAYSCALE)
    if img is None:
        raise RuntimeError(f"cannot read {path}")
    h, w = img.shape
    k = WORK_LONG_EDGE / max(w, h)
    small = cv2.resize(img, (round(w * k), round(h * k)), interpolation=cv2.INTER_AREA)
    return small, 1.0 / k, (w, h)


# ---------------------------------------------------------------- checkerboard corners

def detect_corners(gray):
    """X-corners of a checkerboard: Hessian saddle points, refined with cornerSubPix."""
    g = cv2.GaussianBlur(gray.astype(np.float32), (0, 0), 3.0)
    gxx = cv2.Sobel(g, cv2.CV_32F, 2, 0, ksize=5)
    gyy = cv2.Sobel(g, cv2.CV_32F, 0, 2, ksize=5)
    gxy = cv2.Sobel(g, cv2.CV_32F, 1, 1, ksize=5)
    saddle = gxy * gxy - gxx * gyy
    top = np.percentile(saddle, 99.95)
    if top <= 0:
        return np.zeros((0, 2), np.float32)
    peaks = (saddle == cv2.dilate(saddle, np.ones((15, 15), np.uint8))) & (saddle > 0.08 * top)
    ys, xs = np.nonzero(peaks)
    pts = np.stack([xs, ys], 1).astype(np.float32).reshape(-1, 1, 2)
    if len(pts) == 0:
        return pts.reshape(-1, 2)
    crit = (cv2.TERM_CRITERIA_EPS + cv2.TERM_CRITERIA_MAX_ITER, 50, 0.005)
    pts = cv2.cornerSubPix(gray, pts, (11, 11), (-1, -1), crit).reshape(-1, 2).astype(np.float64)
    # Two peaks near one corner can refine to the same point; keep one of each such pair.
    drop = {j for _, j in cKDTree(pts).query_pairs(3.0)}
    pts = pts[[i for i in range(len(pts)) if i not in drop]]
    return pts[[is_x_corner(g, p) for p in pts]]


def is_x_corner(g, p, r=12):
    """A board corner has alternating quadrants: equal diagonals, contrasting neighbours.
    A point on a straight edge (screen texture can make one look like a saddle) does not."""
    h, w = g.shape
    x, y = int(round(p[0])), int(round(p[1]))
    if not (r <= x < w - r and r <= y < h - r):
        return False
    a, c = g[y + r, x + r], g[y - r, x - r]
    b, d = g[y - r, x + r], g[y + r, x - r]
    contrast = abs((a + c) - (b + d)) / 2
    return contrast > 15 and contrast > 2 * max(abs(a - c), abs(b - d))


def label_lattice(pts, centre):
    """Integer (i, j) grid labels for corners connected to one near the centre.
    i grows along +x, j along +y; local lattice vectors follow the distortion gradually.
    The starting vectors are the consensus of many central corners, so a stray corner (screen
    moiré) next to the start can't derail it; a few starts are tried, the largest result kept."""
    if len(pts) < 20:
        return {}
    tree = cKDTree(pts)
    vectors = lattice_vectors(pts, tree, centre)
    if vectors is None:
        return {}
    best = {}
    for start in tree.query(centre, k=min(9, len(pts)))[1]:
        labels = grow_lattice(pts, tree, int(start), *vectors)
        if len(labels) > len(best):
            best = labels
        if len(best) > 0.8 * len(pts):
            break
    return best if len(best) > 50 else {}


def lattice_vectors(pts, tree, centre):
    """Median neighbour offsets near +x and +y among the corners nearest the centre."""
    k = min(9, len(pts))
    offsets = []
    for i in tree.query(centre, k=min(60, len(pts)))[1]:
        nn = tree.query(pts[i], k=k)[1]
        offsets.extend(pts[nn[1:]] - pts[i])
    offsets = np.array(offsets)
    length = np.linalg.norm(offsets, axis=1)
    spacing = np.median(np.sort(length.reshape(-1, k - 1), axis=1)[:, :4])
    offsets = offsets[np.abs(length - spacing) < 0.25 * spacing]
    angle = np.arctan2(offsets[:, 1], offsets[:, 0])
    near_x = np.abs(np.angle(np.exp(1j * angle))) < np.pi / 4
    near_y = np.abs(np.angle(np.exp(1j * (angle - np.pi / 2)))) < np.pi / 4
    if near_x.sum() < 10 or near_y.sum() < 10:
        return None
    return np.median(offsets[near_x], axis=0), np.median(offsets[near_y], axis=0)


def grow_lattice(pts, tree, start, a, b):
    """Label corners (row, column) by stepping lattice vectors `a` and `b` out from corner `start`."""
    labels = {start: (0, 0)}
    taken = {(0, 0): start}
    vec = {start: (a, b)}
    queue = [start]
    while queue:
        p = queue.pop()
        i, j = labels[p]
        va, vb = vec[p]
        for di, dj, step in ((1, 0, va), (-1, 0, -va), (0, 1, vb), (0, -1, -vb)):
            key = (i + di, j + dj)
            if key in taken:
                continue
            dist, q = tree.query(pts[p] + step)
            if dist > 0.3 * np.linalg.norm(step) or q in labels:
                continue
            found = pts[q] - pts[p]
            na = found * di if di else va
            nb = found * dj if dj else vb
            labels[q] = key
            taken[key] = q
            vec[q] = (na, nb)
            queue.append(q)
    return labels


def board(path):
    """Checkerboard corners of image `path`, keyed by (row, column), in its full-size pixels, and its size."""
    gray, back, size = load_gray(path)
    pts = detect_corners(gray)
    labels = label_lattice(pts, np.array(gray.shape[::-1]) / 2.0)
    full = {lab: pts[k] * back for k, lab in labels.items()}
    return full, size


def straightness(grid):
    """Largest deviation from a fitted straight line over every row/column with 8+ corners."""
    lines = {}
    for (i, j), p in grid.items():
        lines.setdefault(("col", i), []).append(p)
        lines.setdefault(("row", j), []).append(p)
    worst = []
    for pts in lines.values():
        if len(pts) < 8:
            continue
        pts = np.array(pts)
        keep = np.ones(len(pts), bool)
        for _ in range(3):  # drop stray corners, then refit
            c = pts[keep].mean(0)
            _, _, vt = np.linalg.svd(pts[keep] - c)
            dev = (pts - c) @ vt[1]
            keep = np.abs(dev) <= max(1.0, 4 * np.median(np.abs(dev[keep])))
            if keep.sum() < 8:
                break
        if keep.sum() >= 8:
            worst.append(np.abs(dev[keep]).max())
    if not worst:
        return None
    worst = np.array(worst)
    return {"lines": len(worst), "median": float(np.median(worst)), "max": float(worst.max())}


def match_boards(src, src_size, dst, dst_size):
    """Corner pairs between two labellings of the same shot. A board is periodic, so a fit can't
    tell a one-square shift from the right match; the corners near the centre can, unfitted:
    the two framings differ by a few percent of zoom, far less than a square there."""
    sc, dc = np.array(src_size) / 2.0, np.array(dst_size) / 2.0
    half_diag = np.hypot(*dst_size) / 2.0
    best = None
    for oi in range(-3, 4):
        for oj in range(-3, 4):
            pairs = [(p, dst[(i + oi, j + oj)]) for (i, j), p in src.items() if (i + oi, j + oj) in dst]
            central = [np.linalg.norm((p - sc) - (q - dc)) for p, q in pairs if np.linalg.norm(q - dc) < 0.3 * half_diag]
            if len(pairs) < 50 or len(central) < 10:
                continue
            err = float(np.median(central))
            if best is None or err < best[0]:
                best = (err, pairs)
    if best is None:
        return None, None
    s, d = map(np.array, zip(*best[1]))
    return s, d


# ---------------------------------------------------------------- SIFT (other scenes)

def sift_pairs(src_path, dst_path):
    """SIFT matches; both point sets in the destination's full-size pixels."""
    a, ka, size_a = load_gray(src_path)
    b, kb, size_b = load_gray(dst_path)
    sift = cv2.SIFT_create(nfeatures=20000)
    pa, da = sift.detectAndCompute(a, None)
    pb, db = sift.detectAndCompute(b, None)
    if da is None or db is None:
        return None, None
    good = [m for m, n in cv2.BFMatcher().knnMatch(da, db, k=2) if m.distance < 0.75 * n.distance]
    if len(good) < 30:
        return None, None
    s = np.array([pa[m.queryIdx].pt for m in good]) * ka * (size_b[0] / size_a[0])
    d = np.array([pb[m.trainIdx].pt for m in good]) * kb
    return s, d


# ---------------------------------------------------------------- geometry

def fit_similarity(s, d):
    """Least-squares similarity (zoom, rotation, shift) mapping points `s` onto `d`, as complex (scale, offset)."""
    zs = s[:, 0] + 1j * s[:, 1]
    zd = d[:, 0] + 1j * d[:, 1]
    ms, md = zs.mean(), zd.mean()
    c = np.vdot(zs - ms, zd - md) / np.vdot(zs - ms, zs - ms)
    return c, md - c * ms


def apply(model, p):
    """Points `p` mapped by a `fit_similarity` model."""
    c, t = model
    z = c * (p[:, 0] + 1j * p[:, 1]) + t
    return np.stack([z.real, z.imag], 1)


def agreement(s, d, size):
    """Similarity-fit render points s onto JPEG points d; residual statistics by radius."""
    pairs = {tuple(q): p for q, p in zip(d, s)}
    keep = np.ones(len(s), bool)
    # A loose first pass, then reject gross mismatches (SIFT outliers, mislabelled corners).
    for limit in (0.02 * size[0], None, None):
        model = fit_similarity(s[keep], d[keep])
        res = np.linalg.norm(apply(model, s) - d, axis=1)
        cut = limit if limit else max(5 * np.median(res[keep]), 2.0)
        keep = res < cut
        if keep.sum() < 20:
            return None
    model = fit_similarity(s[keep], d[keep])
    s, d = s[keep], d[keep]
    fitted = apply(model, s)
    res = fitted - d
    centre = np.array(size) / 2.0
    half_diag = np.hypot(*size) / 2.0
    rel = d - centre
    r = np.linalg.norm(rel, axis=1) / half_diag
    radial = np.sum(res * rel, axis=1) / np.maximum(np.linalg.norm(rel, axis=1), 1e-9)
    mag = np.linalg.norm(res, axis=1)
    bins = []
    for lo, hi in zip(BINS, BINS[1:]):
        m = (r >= lo) & (r < hi)
        bins.append({"r": f"{lo:.2f}-{min(hi, 1):.2f}", "n": int(m.sum()),
                     "median": float(np.median(mag[m])) if m.any() else None,
                     "radial": float(np.mean(radial[m])) if m.any() else None})
    return {"n": int(len(s)), "scale": float(abs(model[0])), "median": float(np.median(mag)),
            "p95": float(np.percentile(mag, 95)), "max": float(mag.max()), "max_r": float(r.max()),
            "bins": bins, "pairs": pairs}


def correction_fraction(on, off, size):
    """How the camera's correction compares with LightCraft's: JPEG ≈ off + α·(on − off) radially,
    both renders aligned to the JPEG on the same corners. 1 = the same amount as the camera;
    above 1, the camera corrects more than LightCraft."""
    keys = [k for k in on["pairs"] if k in off["pairs"]]
    if len(keys) < 50:
        return None
    d = np.array(keys)
    raw_on = np.array([on["pairs"][k] for k in keys])
    raw_off = np.array([off["pairs"][k] for k in keys])
    keep = np.ones(len(d), bool)
    for _ in range(4):
        a = apply(fit_similarity(raw_on[keep], d[keep]), raw_on)
        b = apply(fit_similarity(raw_off[keep], d[keep]), raw_off)
        rel = d - np.array(size) / 2.0
        unit = rel / np.maximum(np.linalg.norm(rel, axis=1, keepdims=True), 1e-9)
        u = np.sum((a - b) * unit, axis=1)
        v = np.sum((d - b) * unit, axis=1)
        if np.median(np.abs(u[keep])) < 1.0:  # too little distortion at this focal length to tell
            return None
        alpha = float(u[keep] @ v[keep] / (u[keep] @ u[keep]))
        e = np.abs(v - alpha * u)
        keep = e <= max(0.5, 4 * np.median(e[keep]))  # mismatched corners, not the lens
    return alpha


# ---------------------------------------------------------------- driver

def jpeg_for(arw):
    """The camera JPEG next to `arw` (same stem), or None."""
    stem = os.path.splitext(arw)[0]
    for ext in (".JPG", ".jpg", ".JPEG", ".jpeg"):
        if os.path.exists(stem + ext):
            return stem + ext
    return None


def exif_summary(jpg):
    """Lens, focal length and aperture of `jpg` from its Exif, for the report."""
    from PIL import Image
    tags = Image.open(jpg).getexif().get_ifd(0x8769)
    lens = tags.get(0xA434, "?")
    focal = tags.get(0x920A)
    fnum = tags.get(0x829D)
    return f"{lens} {float(focal):.0f}mm f/{float(fnum):.1f}" if focal and fnum else str(lens)


def measure(cli, work, arw):
    """Render `arw` corrected and uncorrected and measure both against its camera JPEG (None without one)."""
    jpg = jpeg_for(arw)
    if not jpg:
        return None
    name = os.path.splitext(os.path.basename(arw))[0]
    # cards repeat names (DSC00001.ARW): the path's hash keeps renders of different folders apart
    uid = hashlib.sha1(os.path.abspath(arw).encode()).hexdigest()[:8]
    on = render(cli, arw, os.path.join(work, f"{name}.{uid}.on.png"), True)
    off = render(cli, arw, os.path.join(work, f"{name}.{uid}.off.png"), False)
    out = {"file": name, "shot": exif_summary(jpg)}
    jgrid, jsize = board(jpg)
    out["kind"] = "board" if jgrid else "scene"
    if jgrid:
        out["straight_jpeg"] = straightness(jgrid)
    for tag, path in (("on", on), ("off", off)):
        if jgrid:
            grid, size = board(path)
            if not grid:
                continue
            k = jsize[0] / size[0]
            grid = {key: p * k for key, p in grid.items()}
            out["straight_" + tag] = straightness(grid)
            s, d = match_boards(grid, (size[0] * k, size[1] * k), jgrid, jsize)
        else:
            s, d = sift_pairs(path, jpg)
        a = agreement(s, d, jsize) if s is not None else None
        if a:
            out["vs_jpeg_" + tag] = a
    if "vs_jpeg_on" in out and "vs_jpeg_off" in out and out["kind"] == "board":
        out["fraction"] = correction_fraction(out["vs_jpeg_on"], out["vs_jpeg_off"], jsize)
    for tag in ("on", "off"):
        out.get("vs_jpeg_" + tag, {}).pop("pairs", None)
    out["jpeg_width"] = jsize[0]
    return out


def show(o):
    """Print one file's measurements."""
    k = REPORT_WIDTH / o["jpeg_width"]
    print(f"\n{o['file']}  {o['shot']}  [{o['kind']}]")
    for tag, label in (("on", "LightCraft corrected"), ("off", "LightCraft uncorrected")):
        a = o.get("vs_jpeg_" + tag)
        if a:
            print(f"  {label:24s} vs camera JPEG: median {a['median']:.2f} px, p95 {a['p95']:.2f}, max {a['max']:.2f}"
                  f"  (@{REPORT_WIDTH}: {a['median'] * k:.2f} / {a['p95'] * k:.2f})  n={a['n']} reach r={a['max_r']:.2f}")
            print("      by radius: " + "  ".join(
                f"{b['r']}: {b['median']:.2f} ({b['radial']:+.2f})" for b in a["bins"] if b["n"]))
    for tag in ("jpeg", "on", "off"):
        st = o.get("straight_" + tag)
        if st:
            print(f"  straightness {tag:4s}: worst line {st['max']:.2f} px, median line {st['median']:.2f} px ({st['lines']} lines)")
    if o.get("fraction") is not None:
        print(f"  camera correction / LightCraft correction: {o['fraction']:.3f}  (1 = same amount)")


def main():
    """Measure every ARW with a camera JPEG in the given folders and print (and optionally save) the results."""
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cli", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--json")
    ap.add_argument("dirs", nargs="+")
    args = ap.parse_args()
    os.makedirs(args.work, exist_ok=True)
    arws = sorted(os.path.join(d, f) for d in args.dirs for f in os.listdir(d) if f.lower().endswith(".arw"))
    results = []
    for arw in arws:
        try:
            o = measure(args.cli, args.work, arw)
        except Exception as e:  # keep going: one bad file must not stop the batch
            print(f"\n{arw}: {e}", file=sys.stderr)
            continue
        if o:
            show(o)
            results.append(o)
    if args.json:
        with open(args.json, "w") as f:
            json.dump(results, f, indent=1)


if __name__ == "__main__":
    main()
