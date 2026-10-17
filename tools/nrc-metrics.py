#!/usr/bin/env python3
"""Compare colour NR exports using spatial quadrants and one fixed reference mask.

PNG comparisons assess the displayed result, not an unclipped float ground truth.
Usage: python3 tools/nrc-metrics.py REF.png VARIANT.png ... [--crop X Y W H]
"""
import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image


def linear(p):
    p = np.asarray(p, dtype=np.float64) / 255.0
    return np.where(p <= 0.04045, p / 12.92, ((p + 0.055) / 1.055) ** 2.4)


def lab(p):
    p = linear(p)
    xyz = p @ np.array([[0.4124, 0.2126, 0.0193], [0.3576, 0.7152, 0.1192], [0.1805, 0.0722, 0.9505]])
    xyz /= np.array([0.95047, 1.0, 1.08883])
    f = np.where(xyz > 0.008856, np.cbrt(xyz), 7.787 * xyz + 16 / 116)
    return np.stack((116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])), axis=-1)


def tiles(reference):
    """Four actual 48x48 quadrants; tiles selected only on the reference."""
    result = []
    for y in range(0, reference.height - 95, 192):
        for x in range(0, reference.width - 95, 192):
            p = linear(reference.crop((x, y, x + 96, y + 96)))[::4, ::4]
            luminance = p @ np.array([0.2126, 0.7152, 0.0722])
            mean = luminance.mean()
            lightness = 116 * (np.cbrt(mean) if mean > 0.008856 else 7.787 * mean + 16 / 116) - 16
            quadrants = luminance.reshape(2, 12, 2, 12).mean(axis=(1, 3))
            if 6 <= lightness <= 25 and np.ptp(quadrants) < 0.15 * mean:
                result.append((x, y))
    return result[:60]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="+")
    parser.add_argument("--crop", nargs=4, type=int, default=[1325, 2085, 360, 240])
    args = parser.parse_args()
    reference = Image.open(args.files[0]).convert("RGB")
    regions = tiles(reference)
    x, y, w, h = args.crop
    box = (x, y, x + w, y + h)
    if x < 0 or y < 0 or w <= 0 or h <= 0 or x + w > reference.width or y + h > reference.height:
        parser.error("crop must lie inside the image")
    ref_lab = lab(reference.crop(box))
    mask = ref_lab[..., 0] > 20
    if not mask.any():
        parser.error("crop contains no reference pixels with L* > 20")
    reference_ab = ref_lab[..., 1:][mask].mean(axis=0)
    reference_c = np.linalg.norm(ref_lab[..., 1:][mask], axis=-1).mean()
    records = []
    for file in args.files:
        image = Image.open(file).convert("RGB")
        if image.size != reference.size:
            parser.error(f"size differs from reference: {file}")
        spreads, blotches, means = [], [], []
        for tx, ty in regions:
            patch = np.asarray(image.crop((tx, ty, tx + 96, ty + 96)))
            ab = lab(patch)[..., 1:]
            spreads.append(np.sqrt(ab.var(axis=(0, 1)).sum()))
            blocks = ab.reshape(12, 8, 12, 8, 2).mean(axis=(1, 3))
            blotches.append(np.sqrt(blocks.var(axis=(0, 1)).sum()))
            means.append(linear(patch).mean(axis=(0, 1)))
        ab = lab(image.crop(box))[..., 1:][mask]
        mean_ab = ab.mean(axis=0)
        records.append({
            "file": str(Path(file)), "flat_tiles": len(regions),
            "spread": float(np.mean(spreads)) if spreads else None,
            "blotch": float(np.mean(blotches)) if blotches else None,
            "dark_rgb": np.mean(means, axis=0).tolist() if means else None,
            "crop_mean_ab": mean_ab.tolist(),
            "crop_mean_colour_delta": float(np.linalg.norm(mean_ab - reference_ab)),
            "crop_mean_chroma_percent": float(100 * np.linalg.norm(mean_ab) / max(np.linalg.norm(reference_ab), 1e-12)),
            "crop_pixel_chroma_percent": float(100 * np.linalg.norm(ab, axis=-1).mean() / max(reference_c, 1e-12)),
            "crop_hue_delta_degrees": float(np.degrees(np.arctan2(np.sin(np.arctan2(mean_ab[1], mean_ab[0]) - np.arctan2(reference_ab[1], reference_ab[0])), np.cos(np.arctan2(mean_ab[1], mean_ab[0]) - np.arctan2(reference_ab[1], reference_ab[0]))))),
        })
    print(json.dumps({"reference": args.files[0], "crop": args.crop, "results": records}, indent=2))


if __name__ == "__main__":
    main()
