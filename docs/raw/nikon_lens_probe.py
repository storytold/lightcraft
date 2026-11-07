#!/usr/bin/env python3
"""Measure Nikon optics without changing LightCraft's decoder or saved edits.

Requires Python with numpy, scipy and Pillow, plus exiftool. Build the companion:
  CARGO_TARGET_DIR=target/agent-nikon-lens cargo build --release -p lightcraft-engine \
    --example nikon_lens_probe
Run on SHA-256-verified, local CC0 corpus files:
  python3 docs/raw/nikon_lens_probe.py --binary target/agent-nikon-lens/release/examples/nikon_lens_probe \
    --out corpus/nikon-lens corpus/raw/coverage-3584.nef

Outputs contain camera images and must remain outside version control. Never point
this at private originals when sharing the output folder. TIFF fields are read
here directly. ExifTool's documentation gives tag identities and offsets; the tool
extracts metadata and embedded JPEG data black-box. No external decoder implementation
is read. Models are hypotheses, not production
corrections. The default render uses the existing camera look; replacing optics
after source loading deliberately does not refit that look.
"""
import argparse
import hashlib
import json
import math
from pathlib import Path
import struct
import subprocess
import sys

import numpy as np
from PIL import Image
from scipy import ndimage, optimize, signal


def json_text(value, **kwargs):
    """Reject non-finite output instead of writing nonstandard JSON NaN/Infinity."""
    return json.dumps(value, allow_nan=False, **kwargs)


def validate_fit_samples(train, radius):
    """Require both tile parities and radial coverage before fitting or averaging.

    At least 250 pairs in each split, and 32 in each inner/middle/outer bin
    of each split, keep empty or tiny regions from masquerading as evidence.
    """
    for name, split in (("training", train), ("validation", ~train)):
        if np.count_nonzero(split) < 250:
            raise ValueError(f"not enough {name} brightness pairs")
        for lo, hi in ((0, 0.35), (0.35, 0.75), (0.75, 0.95)):
            if np.count_nonzero(split & (radius >= lo) & (radius < hi)) < 32:
                raise ValueError(f"not enough {name} pairs in radial bin {lo} to {hi}")


def tiff_entries(data, offset, order):
    """Read one bounded classic-TIFF IFD, without following arbitrary pointers."""
    if offset < 0 or offset + 2 > len(data):
        raise ValueError("invalid IFD offset")
    count = struct.unpack_from(order + "H", data, offset)[0]
    if count > 4096 or offset + 2 + count * 12 > len(data):
        raise ValueError("invalid IFD length")
    sizes = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 9: 4, 10: 8}
    result = {}
    for i in range(count):
        at = offset + 2 + i * 12
        tag, kind, n, value = struct.unpack_from(order + "HHII", data, at)
        if kind not in sizes:
            raise ValueError("unsupported TIFF entry type")
        length = sizes[kind] * n
        start = at + 8 if length <= 4 else value
        if length > 16 * 1024 * 1024 or start + length > len(data):
            raise ValueError("invalid TIFF entry range")
        result[tag] = data[start : start + length]
    return result


def nef_blocks(path):
    """Read NikonNEFInfo through the raw SubIFD, not a decoder's interpretation."""
    data = path.read_bytes()
    order = "<" if data[:2] == b"II" else ">"
    root = tiff_entries(data, struct.unpack_from(order + "I", data, 4)[0], order)
    offsets = root.get(330, b"")
    if len(offsets) % 4 or len(offsets) > 128:
        raise ValueError("invalid SubIFD offsets")
    for at in struct.unpack(order + "I" * (len(offsets) // 4), offsets):
        blob = tiff_entries(data, at, order).get(0xC7D5)
        if blob is not None:
            if not blob.startswith(b"Nikon\0"):
                raise ValueError("unrecognized NikonNEFInfo")
            inner = blob[10:]
            order = "<" if inner[:2] == b"II" else ">"
            entries = tiff_entries(
                inner, struct.unpack_from(order + "I", inner, 4)[0], order
            )

            def rat(block, at):
                n, d = struct.unpack_from(order + "ii", block, at)
                if d <= 0:
                    raise ValueError("invalid signed rational")
                return n / d

            distortion, vignette, center = (entries[k] for k in (5, 6, 8))
            for name, block, minimum in (
                ("DistortionInfo", distortion, 72),
                ("VignetteInfo", vignette, 104),
                ("optical center", center, 20),
            ):
                if len(block) < minimum:
                    raise ValueError(f"truncated {name} block: need {minimum} bytes")
            return dict(
                distortion_version=distortion[:4].decode("ascii"),
                distortion_mode=distortion[4],
                distortion_radius=rat(distortion, 8),
                distortion_k=[rat(distortion, at) for at in (20, 28, 36)],
                zoom=rat(distortion, 64),
                center=[rat(center, at) for at in (4, 12)],
                vignette_version=vignette[:4].decode("ascii"),
                vignette_mode=vignette[4],
                vignette_radius=rat(vignette, 8),
                vignette_terms=struct.unpack_from(order + "I", vignette, 16)[0],
                vignette_polynomial=[rat(vignette, at) for at in range(20, 92, 8)],
                vignette_tail=rat(vignette, 96),
                blocks={str(k): entries[k].hex() for k in (5, 6, 8)},
            )
    raise ValueError("NikonNEFInfo absent")


def proxy(folder):
    """Read the companion example's little-endian camera RGB float dump."""
    data = (folder / "camera-rgb.f32").read_bytes()
    w, h = struct.unpack_from("<II", data)
    if not w or not h or w * h > 16_000_000 or len(data) != 8 + w * h * 12:
        raise ValueError("invalid proxy dimensions or length")
    return np.frombuffer(data, "<f4", offset=8).reshape(h, w, 3).astype(float)


def coordinates(raw, meta, shape):
    """Stored distortion hypothesis, output coordinates to unwarped crop coordinates."""
    h, w = shape[:2]
    crop = raw["crop"]
    cw, ch = crop["width"], crop["height"]
    yy, xx = np.mgrid[:h, :w]
    xx = (xx + 0.5) * cw / w - 0.5
    yy = (yy + 0.5) * ch / h - 0.5
    cx, cy = (cw - 1) / 2 + meta["center"][0], (ch - 1) / 2 + meta["center"][1]
    hd = math.hypot(cw - 1, ch - 1) / 2
    r2 = ((xx - cx) ** 2 + (yy - cy) ** 2) / hd**2
    k1, k2, k3 = meta["distortion_k"]
    factor = (1 + r2 * (k3 + r2 * (k2 + r2 * k1))) / (1 + meta["zoom"])
    sx, sy = cx + (xx - cx) * factor, cy + (yy - cy) * factor
    return sx, sy, ((sx - cx) ** 2 + (sy - cy) ** 2) / hd**2


def sample(image, sx, sy):
    """Sample a camera-RGB proxy using full-resolution crop pixel coordinates."""
    x = (sx + 0.5) / 4 - 0.5
    y = (sy + 0.5) / 4 - 0.5
    return np.stack(
        [
            ndimage.map_coordinates(image[..., c], [y, x], order=1, mode="nearest")
            for c in range(3)
        ],
        axis=-1,
    )


def lstar(image):
    """CIE Lab L* of an sRGB image, D65 white, independent of chroma adaptation."""
    x = np.asarray(image, float) / 255
    x = np.where(x <= 0.04045, x / 12.92, ((x + 0.055) / 1.055) ** 2.4)
    y = x @ np.array([0.2126729, 0.7151522, 0.0721750])
    return np.where(y > 216 / 24389, 116 * np.cbrt(y) - 16, y * 24389 / 27)


def score(render, reference):
    """Pearson and signed render-minus-reference dL, all pixels and outer ring."""
    # The old sweep's alignment script is absent from this checkout. Here both frames have
    # the camera's aspect ratio, normal orientation, and are resized directly.
    size = (200, round(reference.height * 200 / reference.width))
    a = lstar(render.convert("RGB").resize(size, Image.Resampling.LANCZOS))
    b = lstar(reference.convert("RGB").resize(size, Image.Resampling.LANCZOS))
    y, x = np.mgrid[: a.shape[0], : a.shape[1]]
    rho = (
        np.hypot((x + 0.5 - a.shape[1] / 2), (y + 0.5 - a.shape[0] / 2))
        / math.hypot(*a.shape)
        * 2
    )
    ring = rho >= 0.75
    if np.std(a) < 1e-8 or np.std(b) < 1e-8:
        raise ValueError("cannot correlate a constant image")
    delta = a - b
    return dict(
        r=float(np.corrcoef(a.ravel(), b.ravel())[0, 1]),
        mean_dl=float(delta.mean()),
        outer_mean_dl=float(delta[ring].mean()),
        outer_mae_dl=float(np.abs(delta[ring]).mean()),
        outer_pixels=int(ring.sum()),
    )


def patches(camera, jpeg, raw, meta):
    """NCC patch matches with subpixel peaks; no fitted geometry parameters."""
    h, w = camera.shape[:2]
    reference = np.asarray(jpeg.resize((w, h), Image.Resampling.LANCZOS), float)[..., 1]
    source = np.log(np.maximum(camera[..., 1], 1e-6))
    reference = np.log(np.maximum(reference, 1))
    sx, sy, _ = coordinates(raw, meta, camera.shape)
    records = []
    radius, search = 9, 12
    for y in np.linspace(24, h - 25, 19).astype(int):
        for x in np.linspace(24, w - 25, 27).astype(int):
            px, py = (sx[y, x] + 0.5) / 4 - 0.5, (sy[y, x] + 0.5) / 4 - 0.5
            ix, iy = round(px), round(py)
            if (
                ix - search - radius < 0
                or iy - search - radius < 0
                or ix + search + radius >= w
                or iy + search + radius >= h
            ):
                continue
            template = reference[
                y - radius : y + radius + 1, x - radius : x + radius + 1
            ]
            template = template - template.mean()
            if np.std(template) < 0.025:
                continue
            region = source[
                iy - search - radius : iy + search + radius + 1,
                ix - search - radius : ix + search + radius + 1,
            ]
            n = template.size
            ones = np.ones(template.shape)
            sums = signal.correlate2d(region, ones, mode="valid")
            squares = signal.correlate2d(region**2, ones, mode="valid")
            cc = signal.correlate2d(region, template, mode="valid") / np.sqrt(
                np.maximum((squares - sums**2 / n) * np.sum(template**2), 1e-20)
            )
            by, bx = np.unravel_index(np.argmax(cc), cc.shape)
            peak = cc[by, bx]
            cc_other = cc.copy()
            cc_other[max(0, by - 3) : by + 4, max(0, bx - 3) : bx + 4] = -1
            if (
                peak < 0.9
                or peak - cc_other.max() < 0.02
                or bx in (0, 2 * search)
                or by in (0, 2 * search)
            ):
                continue

            def sub(a, b, c):
                denominator = a - 2 * b + c
                return (
                    float(np.clip(0.5 * (a - c) / denominator, -0.5, 0.5))
                    if denominator < -1e-8
                    else 0
                )

            dx = sub(cc[by, bx - 1], peak, cc[by, bx + 1])
            dy = sub(cc[by - 1, bx], peak, cc[by + 1, bx])
            mx, my = ix + bx - search + dx, iy + by - search + dy
            records.append([x, y, px, py, mx, my, float(peak)])
    points = np.asarray(records)
    if len(points) < 12:
        raise ValueError("too few reliable patch matches")
    errors = np.hypot(points[:, 4] - points[:, 2], points[:, 5] - points[:, 3]) * 4
    before = np.hypot(points[:, 4] - points[:, 0], points[:, 5] - points[:, 1]) * 4
    return dict(
        matches=len(points),
        before_median_px=float(np.median(before)),
        before_p95_px=float(np.quantile(before, 0.95)),
        after_median_px=float(np.median(errors)),
        after_p95_px=float(np.quantile(errors, 0.95)),
        points=points.tolist(),
        **geometry_details(points, raw, meta),
    )


def geometry_details(points, raw, meta):
    """Radial residuals, reversed-order control, and an independently fitted zoom."""
    points = np.asarray(points, dtype=float)
    if points.ndim != 2 or points.shape[1] < 6 or not np.isfinite(points).all():
        raise ValueError("invalid or non-finite geometry patch positions")
    cw, ch = raw["crop"]["width"] - 1, raw["crop"]["height"] - 1
    numbers = np.asarray([cw, ch, *meta["center"], *meta["distortion_k"], meta["zoom"]])
    if not np.isfinite(numbers).all():
        raise ValueError("non-finite geometry dimensions or lens coefficients")
    if cw <= 0 or ch <= 0:
        raise ValueError("geometry needs an image area wider and taller than one pixel")
    zoom_denominator = 1 + meta["zoom"]
    if zoom_denominator <= 0:
        raise ValueError("geometry zoom denominator 1 + zoom must be positive")
    cx, cy = cw / 2 + meta["center"][0], ch / 2 + meta["center"][1]
    dx = (points[:, 0] + 0.5) * 4 - 0.5 - cx
    dy = (points[:, 1] + 0.5) * 4 - 0.5 - cy
    mx = (points[:, 4] + 0.5) * 4 - 0.5 - cx
    my = (points[:, 5] + 0.5) * 4 - 0.5 - cy
    length = np.hypot(dx, dy)
    rho = length / (math.hypot(cw, ch) / 2)
    measured = (mx * dx + my * dy) / np.maximum(length, 1e-9)
    k1, k2, k3 = meta["distortion_k"]
    x = rho**2
    unscaled = length * (1 + x * (k3 + x * (k2 + x * k1)))
    predicted = unscaled / zoom_denominator
    reversed_order = length * (1 + x * (k1 + x * (k2 + x * k3))) / zoom_denominator
    if not all(np.isfinite(array).all() for array in (rho, measured, unscaled, predicted, reversed_order)):
        raise ValueError("non-finite geometry radii or distortion predictions")
    use = (rho > 0.15) & (rho < 0.95)
    count = int(np.count_nonzero(use))
    if count < 20:
        raise ValueError(f"too few radial geometry matches: {count}; need at least 20 with 0.15 < rho < 0.95")
    norm = float(np.dot(unscaled[use], unscaled[use]))
    projected = float(np.dot(unscaled[use], measured[use]))
    if not math.isfinite(norm) or not math.isfinite(projected) or norm <= 0 or projected <= 0:
        raise ValueError("geometry zoom fit has a zero, negative or non-finite denominator; check matched radii")
    zoom = norm / projected - 1
    radial = np.abs(measured[use] - predicted[use])
    result = dict(
        before_radial_median_px=float(np.median(np.abs(measured[use] - length[use]))),
        before_radial_p95_px=float(
            np.quantile(np.abs(measured[use] - length[use]), 0.95)
        ),
        radial_median_px=float(np.median(radial)),
        radial_p95_px=float(np.quantile(radial, 0.95)),
        reversed_order_radial_median_px=float(
            np.median(np.abs(measured[use] - reversed_order[use]))
        ),
        stored_zoom=meta["zoom"],
        fitted_zoom=float(zoom),
        radius_min=float(rho.min()),
        radius_max=float(rho.max()),
    )
    if not all(math.isfinite(value) for value in result.values()):
        raise ValueError("non-finite geometry fit statistics")
    return result


def vignette_fit(camera, jpeg, raw, meta):
    """Compare gain hypotheses with a nuisance tone curve and spatial holdout.

    A spline absorbs global Picture Control contrast. Colour ratios absorb the
    first-order camera-to-JPEG colour transform. No radial term enters that fit.
    Alternate 24-pixel tiles are withheld. The fitted gain strength is diagnostic
    only, never a per-file production adjustment. Scene/radius confounding remains
    possible, especially on the Auto Picture Control samples.
    """
    w = 480
    h = round(jpeg.height * w / jpeg.width)
    sx, sy, r2 = coordinates(raw, meta, (h, w))
    sensor = sample(camera, sx, sy)
    rgb = np.asarray(jpeg.resize((w, h), Image.Resampling.LANCZOS), float) / 255
    target = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    yy, xx = np.mgrid[:h, :w]
    mask = (
        (rgb.min(axis=-1) > 0.04)
        & (rgb.max(axis=-1) < 0.95)
        & (sensor.min(axis=-1) > 0.0002)
        & (sensor.max(axis=-1) < 0.9)
        & (r2 < 0.95**2)
    )
    # Smooth edges can compare different subjects when geometry is imperfect.
    grad = np.hypot(
        ndimage.sobel(np.log(np.maximum(target[..., 1], 1e-5)), 0),
        ndimage.sobel(np.log(np.maximum(target[..., 1], 1e-5)), 1),
    )
    mask &= grad < 1.0
    train = ((xx // 24 + yy // 24) % 2 == 0)[mask]
    radius = np.sqrt(r2[mask])
    green = np.log(sensor[..., 1][mask])
    response = np.log(target[..., 1][mask])
    colours = np.log(
        np.maximum(sensor[..., [0, 2]][mask], 1e-8) / sensor[..., 1][mask, None]
    )
    red, blue = colours.T
    poly = meta["vignette_polynomial"]
    x = r2[mask]
    complete = np.polyval(poly, np.sqrt(x))
    named = 1 + x * (poly[6] + x * (poly[4] + x * poly[2]))
    validate_fit_samples(train, radius)
    if not all(np.isfinite(a).all() for a in (green, response, colours, complete)):
        raise ValueError("non-finite brightness pairs or gain")
    if not np.all(complete > 0):
        raise ValueError("not enough valid brightness pairs or nonpositive gain")

    def fit(gain):
        if not np.isfinite(gain).all() or not np.all(gain > 0):
            raise ValueError("non-finite or nonpositive candidate gain")
        corrected = green + np.log(gain)
        knots = np.linspace(-11, 1, 17)
        design = np.stack(
            [np.ones_like(green), corrected]
            + [np.maximum(corrected - k, 0) for k in knots[1:-1]]
            + [red, blue, red * blue, red**2, blue**2],
            axis=-1,
        )
        coef = np.linalg.lstsq(design[train], response[train], rcond=1e-8)[0]
        residual = response - design @ coef
        test = ~train
        ring = test & (radius >= 0.75)
        inner = test & (radius <= 0.35)
        statistics = dict(
            training_log_rmse=float(np.sqrt(np.mean(residual[train] ** 2))),
            heldout_log_rmse=float(np.sqrt(np.mean(residual[test] ** 2))),
            outer_log_bias=float(np.mean(residual[ring])),
            inner_log_bias=float(np.mean(residual[inner])),
            outer_minus_inner=float(np.mean(residual[ring]) - np.mean(residual[inner])),
        )
        if not all(math.isfinite(value) for value in statistics.values()):
            raise ValueError("non-finite brightness fit statistics")
        return statistics

    candidates = {
        "none": np.ones_like(x),
        "complete": complete,
        "named_three": named,
        "reciprocal": 1 / complete,
        "sqrt": np.sqrt(complete),
        "squared": complete**2,
        "tail_strength": 1 + meta["vignette_tail"] * (complete - 1),
        "stops": 2 ** (complete - 1),
    }
    result = {name: fit(gain) for name, gain in candidates.items()}
    fitted = optimize.minimize_scalar(
        lambda a: fit(1 + a * (complete - 1))["training_log_rmse"],
        bounds=(0, 2),
        method="bounded",
        options={"xatol": 0.01},
    )
    if not fitted.success or not math.isfinite(fitted.x):
        raise ValueError("brightness strength optimization did not converge")
    result["fitted_strength"] = dict(
        alpha=float(fitted.x), **fit(1 + fitted.x * (complete - 1))
    )
    result["samples"] = len(green)
    result["outer_samples"] = int(np.sum((~train) & (radius >= 0.75)))
    result["gains_at_rho"] = {
        str(r): float(np.polyval(poly, r)) for r in (0.25, 0.5, 0.75, 1)
    }
    return result


def opcodes(raw, meta, vignette=False, strength=1.0):
    """Convert candidate models to the existing active-area OpcodeList3 convention."""
    crop, active = raw["crop"], raw["active_area"]
    cw, ch = crop["width"] - 1, crop["height"] - 1
    ax, ay = active["width"] - 1, active["height"] - 1
    cx, cy = (
        crop["x"] + cw / 2 + meta["center"][0],
        crop["y"] + ch / 2 + meta["center"][1],
    )
    half = math.hypot(cw, ch) / 2
    a = math.hypot(max(cx, ax - cx), max(cy, ay - cy)) / half
    k1, k2, k3 = meta["distortion_k"]
    s = 1 / (1 + meta["zoom"])
    result = [
        {
            "WarpRectilinear": dict(
                planes=[[s, k3 * a * a * s, k2 * a**4 * s, k1 * a**6 * s, 0, 0]],
                center=[cx / ax, cy / ay],
            )
        }
    ]
    if vignette:
        poly = meta["vignette_polynomial"]
        result.insert(
            0,
            {
                "FixVignetteRadial": dict(
                    k=[
                        strength * poly[6] * a * a,
                        strength * poly[4] * a**4,
                        strength * poly[2] * a**6,
                        strength * poly[0] * a**8,
                        0,
                    ],
                    center=[cx / ax, cy / ay],
                )
            },
        )
    return result


def prepare(path, folder, binary):
    """Write diagnostics beside verified public inputs, without touching originals."""
    folder.mkdir(parents=True, exist_ok=True)
    previous = folder / "metadata.json"
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    if previous.exists() and json.loads(previous.read_text()).get("sha256") != digest:
        raise ValueError("output folder belongs to another input; use a fresh folder")
    subprocess.run([str(binary), str(path), str(folder)], check=True)
    tags = [
        "Model",
        "LensModel",
        "FocalLength",
        "FNumber",
        "DateTimeOriginal",
        "ShutterCount",
        "VignetteControl",
        "PictureControlName",
        "ActiveD-Lighting",
        "AutoDistortionControl",
        "ColorSpace",
    ]
    exif = json.loads(
        subprocess.check_output(
            ["exiftool", "-j", "-n"] + ["-" + t for t in tags] + [str(path)]
        )
    )[0]
    exif.pop("SourceFile", None)
    if exif.get("ColorSpace") != 1:
        raise ValueError("this probe currently requires an sRGB camera JPEG")
    meta = nef_blocks(path)
    meta.update(exif=exif, sha256=hashlib.sha256(path.read_bytes()).hexdigest())
    (folder / "metadata.json").write_text(json_text(meta, indent=2) + "\n")
    if not (folder / "camera.jpg").exists():
        data = subprocess.check_output(["exiftool", "-b", "-JpgFromRaw", str(path)])
        if not data:
            raise ValueError("full-size embedded camera JPEG absent")
        (folder / "camera.jpg").write_bytes(data)
    raw = json.loads((folder / "raw.json").read_text())
    crop = raw["crop"]
    if crop["width"] % 4 or crop["height"] % 4:
        raise ValueError("this probe requires crop dimensions divisible by four")
    if raw["orientation"] != "Normal":
        raise ValueError("this probe currently requires normal EXIF orientation")
    with Image.open(folder / "camera.jpg") as jpeg:
        if jpeg.size != (crop["width"], crop["height"]):
            raise ValueError("camera JPEG does not cover the full default crop")
    for name, vig, strength in [
        ("distortion", False, 1),
        ("vignette", True, 1),
        ("vignette-tail", True, meta["vignette_tail"]),
    ]:
        ops = folder / (name + ".json")
        ops.write_text(json_text(opcodes(raw, meta, vig, strength), indent=2) + "\n")
    return raw, meta


def main():
    """Prepare data and report baseline and geometry, leaving production untouched."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument(
        "--render-candidates",
        action="store_true",
        help="render the stored polynomial hypotheses after source loading",
    )
    parser.add_argument("files", type=Path, nargs="+")
    args = parser.parse_args()
    for path in args.files:
        folder = args.out / path.stem
        manifest = Path(__file__).with_name("coverage-sweep.tsv")
        for line in manifest.read_text().splitlines():
            cols = line.split("\t")
            if (
                len(cols) == 14
                and cols[3] == path.name
                and hashlib.sha256(path.read_bytes()).hexdigest() != cols[5]
            ):
                raise ValueError("input differs from pinned coverage-sweep SHA-256")
        raw, meta = prepare(path, folder, args.binary)
        camera = proxy(folder)
        jpeg = Image.open(folder / "camera.jpg").convert("RGB")
        result = dict(
            sha256=meta["sha256"],
            exif=meta["exif"],
            baseline=score(Image.open(folder / "baseline.png"), jpeg),
            distortion=patches(camera, jpeg, raw, meta),
            vignette=vignette_fit(camera, jpeg, raw, meta),
        )
        if args.render_candidates:
            scores = {}
            for name in ("distortion", "vignette", "vignette-tail"):
                subprocess.run(
                    [
                        str(args.binary),
                        str(path),
                        str(folder),
                        str(folder / (name + ".json")),
                    ],
                    check=True,
                )
                (folder / "candidate.png").replace(folder / (name + ".png"))
                scores[name] = score(Image.open(folder / (name + ".png")), jpeg)
            result["experimental_renders_no_look_refit"] = scores
        (folder / "measurements.json").write_text(json_text(result, indent=2) + "\n")
        print(
            path.name,
            json_text(
                dict(
                    baseline=result["baseline"],
                    fitted_strength=result["vignette"]["fitted_strength"],
                )
            ),
            flush=True,
        )
        print(
            "distortion",
            json_text({k: v for k, v in result["distortion"].items() if k != "points"}),
            flush=True,
        )


if __name__ == "__main__":
    try:
        main()
    except (
        ValueError,
        OSError,
        KeyError,
        struct.error,
        subprocess.CalledProcessError,
    ) as error:
        print(f"nikon_lens_probe: {error}", file=sys.stderr)
        sys.exit(1)
