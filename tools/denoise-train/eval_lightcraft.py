"""Compare AI Denoise with LightCraft's manual noise reduction, end to end, on synthetic noisy/clean raw pairs.

Offline tooling; not part of LightCraft.

For each clean source raw and noise level, `rawdump noisy` writes two DNGs: the raw darkened by `k` with
calibrated Poisson-Gaussian noise, and the same without noise (the reference). `lightcraft-cli` develops them with
the same settings (exposure brought back by log2(1/k), no sharpening):

- reference: the clean DNG, no noise reduction;
- none: the noisy DNG, no noise reduction;
- manual-L: the noisy DNG with Luminance L (several L, the best is reported) and Color 25;
- ai: the noisy DNG with AI Denoise at 100 (installed from `--model`), Luminance and Color 0.

PSNR and SSIM are computed on the 8-bit sRGB exports against the reference, and same-place crops are saved
side by side. Everything is written under `--out` (keep it out of the repository: the sources may be private).

    python eval_lightcraft.py --cli <lightcraft-cli> --rawdump <rawdump> --model <folder> --out <dir> RAW [RAW ...]
"""

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import time

import numpy as np
from PIL import Image

LEVELS = {  # name: (a, b, k) -- EOS R5 C calibration (a = 2.1e-7 x ISO), extrapolated above ISO 3200
    "iso3200": (6.6e-4, 1.25e-6, 1.0),
    "iso12800": (2.65e-3, 2.0e-5, 1.0),
    "iso6400-pushed2ev": (1.32e-3, 5.0e-6, 0.25),
}
MANUAL = (30, 60, 100)


def run(cmd, env=None, timeout=1800):
    p = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=timeout)
    if p.returncode != 0:
        raise RuntimeError(f"{cmd[:3]} failed: {p.stdout[-800:]} {p.stderr[-800:]}")
    return p.stdout


def render(cli, env, lib, dng, values, out_png, denoise=False):
    if os.path.exists(lib):
        shutil.rmtree(lib)
    cmds = [cli, "run", "--library", lib, "--import", dng]
    cmds += ["develop.set", "values=" + json.dumps(values)]
    if denoise:
        cmds += ["develop.set", "control=enhance.denoise", "value=100"]
    cmds += ["app.export", f"path={out_png}", "longEdge=0", "format=png"]
    t = time.time()
    out = run(cmds, env)
    return time.time() - t, out


def load(p):
    return np.asarray(Image.open(p).convert("RGB"), dtype=np.float32) / 255.0


def psnr(a, b):
    mse = float(((a - b) ** 2).mean())
    return 10 * math.log10(1.0 / max(mse, 1e-12))


def ssim(a, b):
    """Mean SSIM on luma with an 8 x 8 box window (stride 4), constants of Wang et al. 2004."""
    ya, yb = a @ np.array([0.299, 0.587, 0.114], np.float32), b @ np.array([0.299, 0.587, 0.114], np.float32)
    c1, c2 = 0.01 ** 2, 0.03 ** 2
    h, w = (ya.shape[0] // 4) * 4, (ya.shape[1] // 4) * 4

    def box(x):
        x = x[:h, :w]
        s = x.reshape(h // 4, 4, w // 4, 4).sum((1, 3))
        s = s[:-1, :-1] + s[1:, :-1] + s[:-1, 1:] + s[1:, 1:]
        return s / 64.0

    ma, mb = box(ya), box(yb)
    va, vb, cab = box(ya * ya) - ma * ma, box(yb * yb) - mb * mb, box(ya * yb) - ma * mb
    s = ((2 * ma * mb + c1) * (2 * cab + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2))
    return float(s.mean())


def crop_box(ref, side=384):
    """A textured spot (largest local gradient energy on a coarse grid), the same for every variant."""
    y = ref @ np.array([0.299, 0.587, 0.114], np.float32)
    g = np.abs(np.diff(y, axis=0))[:, :-1] + np.abs(np.diff(y, axis=1))[:-1, :]
    best, at = -1, (0, 0)
    for yy in range(0, g.shape[0] - side, side // 2):
        for xx in range(0, g.shape[1] - side, side // 2):
            region = y[yy:yy + side, xx:xx + side]
            if region.mean() < 0.15:
                continue
            e = float(g[yy:yy + side, xx:xx + side].mean())
            if e > best:
                best, at = e, (yy, xx)
    return at[0], at[1], side


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cli", required=True)
    ap.add_argument("--rawdump", required=True)
    ap.add_argument("--model", required=True, help="folder with model.onnx and denoise-model.json")
    ap.add_argument("--out", required=True)
    ap.add_argument("--run-on", default="auto")
    ap.add_argument("sources", nargs="+")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    env = dict(os.environ)
    models = os.path.join(a.out, "denoise-models")
    env["LIGHTCRAFT_DENOISE_MODELS"] = models
    if os.path.exists(models):
        shutil.rmtree(models)
    print(run([a.cli, "run", "denoise.models.install", "path=" + os.path.join(a.model, "model.onnx"), "acknowledged=true",
               "denoise.settings", f"runOn={a.run_on}"], env)[-600:])
    base = {"detail.sharpenAmount": 0, "detail.nrLuminance": 0, "detail.nrColor": 0}
    results = []
    for si, src in enumerate(a.sources):
        for name, (aa, bb, k) in LEVELS.items():
            tag = f"s{si}-{name}"
            d = os.path.join(a.out, tag)
            os.makedirs(d, exist_ok=True)
            noisy, clean = os.path.join(d, "noisy.dng"), os.path.join(d, "clean.dng")
            run([a.rawdump, "noisy", src, noisy, str(aa), str(bb), str(k)])
            run([a.rawdump, "noisy", src, clean, "0", "0", str(k)])
            ev = math.log2(1 / k)
            vals = dict(base, **{"light.exposure": ev})
            lib = os.path.join(a.out, "lib")
            pngs = {}
            render(a.cli, env, lib, clean, vals, os.path.join(d, "reference.png"))
            pngs["reference"] = os.path.join(d, "reference.png")
            render(a.cli, env, lib, noisy, vals, os.path.join(d, "none.png"))
            pngs["none"] = os.path.join(d, "none.png")
            for L in MANUAL:
                p = os.path.join(d, f"manual-{L}.png")
                render(a.cli, env, lib, noisy, dict(vals, **{"detail.nrLuminance": L, "detail.nrColor": 25}), p)
                pngs[f"manual-{L}"] = p
            p = os.path.join(d, "ai.png")
            secs, _ = render(a.cli, env, lib, noisy, vals, p, denoise=True)
            pngs["ai"] = p
            ref = load(pngs["reference"])
            row = {"source": si, "level": name, "aiRenderSeconds": round(secs, 1)}
            imgs = {}
            for kname, path in pngs.items():
                img = load(path)
                imgs[kname] = img
                if kname != "reference":
                    row[kname] = {"psnr": round(psnr(img, ref), 2), "ssim": round(ssim(img, ref), 4)}
            best = max((n for n in row if n.startswith("manual-")), key=lambda n: row[n]["psnr"])
            row["bestManual"] = best
            y, x, s = crop_box(ref)
            order = ["reference", "none", best, "ai"]
            strip = np.concatenate([imgs[n][y:y + s, x:x + s] for n in order], axis=1)
            Image.fromarray((np.clip(strip, 0, 1) * 255).round().astype(np.uint8)).save(os.path.join(d, "crops.png"))
            row["crops"] = os.path.join(d, "crops.png") + "  (" + " | ".join(order) + ")"
            print(json.dumps(row), flush=True)
            results.append(row)
            for f in (noisy, clean):
                os.remove(f)
    json.dump(results, open(os.path.join(a.out, "results.json"), "w"), indent=1)


if __name__ == "__main__":
    sys.exit(main())
