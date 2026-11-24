#!/usr/bin/env python3
"""Compare production CLI exports across all 40 pinned coverage-sweep inputs.

Run from the repository root with Python, numpy, scipy, Pillow and exiftool:
  python3 docs/raw/coverage_render_sweep.py --cli BASE_CLI --out corpus/nikon-lens/base
  python3 docs/raw/coverage_render_sweep.py --cli CANDIDATE_CLI --out corpus/nikon-lens/candidate \
    --compare corpus/nikon-lens/base/scores.json

Use a fresh output directory per binary. The second run reuses the first run's
reference images and framing. Existing PNGs allow an interrupted run to resume.
Inputs are only the SHA-256-pinned public corpus rows. Images and binary copies
stay in gitignored corpus; commit the numerical evidence separately. PreviewTIFF
rows use encoded-luminance correlation for structure only, with no sRGB transfer
or colour-error metrics. Older PreviewTIFF baseline scores must be regenerated
with this tool; valid cached PNGs can be reused.
Cache filenames include the input SHA-256. Legacy stem-only caches are ignored.
"""
import argparse, csv, hashlib, io, json, math, os, subprocess, sys
from pathlib import Path
import numpy as np
from PIL import Image, ImageOps
from nikon_lens_probe import score


def readable_png(path):
    """Only a complete, decodable PNG can stand in for a completed render."""
    try:
        with Image.open(path) as image:
            if image.format != "PNG":
                return False
            image.verify()
        with Image.open(path) as image:
            image.load()
        return True
    except (OSError, ValueError, SyntaxError, Image.DecompressionBombError):
        return False


def measure_pair(render, reference, structure_only):
    if not structure_only:
        return score(render, reference)
    # Unknown TIFF encoding: compare spatial luminance without assuming an sRGB transfer function.
    if reference.width <= 0 or reference.height <= 0:
        raise ValueError("structure reference dimensions must be positive")
    height = round(reference.height * 200 / reference.width)
    if not 1 <= height <= 2000:
        raise ValueError("structure reference aspect ratio exceeds the scoring limit")
    size = (200, height)
    a, b = [
        np.asarray(ImageOps.grayscale(image).resize(size, Image.Resampling.LANCZOS), dtype=float)
        for image in (render, reference)
    ]
    if np.std(a) < 1e-8 or np.std(b) < 1e-8:
        raise ValueError("cannot correlate a constant structure image")
    return dict(r=float(np.corrcoef(a.ravel(), b.ravel())[0, 1]))


def compare_record(name, record, base):
    """Compare scores only after confirming that both runs measured the same inputs."""
    if not isinstance(base, dict):
        raise ValueError("baseline record missing or invalid: " + name)
    for key in ("file_sha256", "reference_sha256", "reference_size", "reference", "rotation"):
        if base.get(key) != record[key]:
            raise ValueError("baseline " + key + " differs: " + name)
    if base.get("metric_kind", "lab_lstar") != record["metric_kind"]:
        raise ValueError("baseline metric_kind differs; regenerate baseline scores: " + name)
    keys = ("r", "mean_dl", "outer_mae_dl") if record["metric_kind"] == "lab_lstar" else ("r",)
    for key in keys:
        value = base.get(key)
        if type(value) not in (int, float) or not math.isfinite(value):
            raise ValueError("baseline " + key + " is missing or non-finite: " + name)
    if not isinstance(base.get("pixel_sha256"), str):
        raise ValueError("baseline pixel_sha256 is missing or invalid: " + name)
    record["pixels_changed"] = record["pixel_sha256"] != base["pixel_sha256"]
    record["delta_r"] = record["r"] - base["r"]
    if record["metric_kind"] == "lab_lstar":
        record["delta_abs_mean_dl"] = abs(record["mean_dl"]) - abs(base["mean_dl"])


def metrics_regressed(record, base):
    if record["r"] < base["r"] - 1e-9:
        return True
    return record["metric_kind"] == "lab_lstar" and (
        abs(record["mean_dl"]) > abs(base["mean_dl"]) + 1e-9
        or record["outer_mae_dl"] > base["outer_mae_dl"] + 1e-9
    )


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--cli", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--compare", type=Path)
    a = p.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    rows = list(
        csv.DictReader(
            [
                l
                for l in Path("docs/raw/coverage-sweep.tsv").read_text().splitlines()
                if not l.startswith("#")
            ],
            delimiter="\t",
        )
    )
    previous = json.loads(a.compare.read_text())["records"] if a.compare else None
    if previous is not None and (
        not isinstance(previous, dict) or set(previous) != {row["file"] for row in rows}
    ):
        raise ValueError("baseline input files differ from the coverage manifest")
    empty = Path("corpus/nikon-lens/empty-lensdb").resolve()
    empty.mkdir(parents=True, exist_ok=True)
    env = dict(
        os.environ,
        LIGHTCRAFT_GPU="0",
        LIGHTCRAFT_LENSFUN_DIR=str(empty),
        RAYON_NUM_THREADS="6",
    )
    results = {}
    cli_sha = hashlib.sha256(a.cli.read_bytes()).hexdigest()
    record_path = a.out / "run.json"
    configuration = dict(
        cli_sha256=cli_sha,
        compare=str(a.compare.resolve()) if a.compare else None,
        rayon_threads=6,
        gpu=False,
    )
    if record_path.exists() and json.loads(record_path.read_text()) != configuration:
        raise ValueError(
            "output directory belongs to a different binary or comparison; use a fresh directory"
        )
    record_path.write_text(json.dumps(configuration, indent=2, allow_nan=False) + "\n")
    for row in rows:
        raw = Path("corpus/raw") / row["file"]
        digest = hashlib.sha256(raw.read_bytes()).hexdigest()
        if digest != row["sha256"]:
            raise ValueError("SHA-256 mismatch " + row["file"])
        cache_stem = raw.stem + "-" + digest
        out = a.out / (cache_stem + ".png")
        ref = (a.compare.parent if a.compare else a.out) / (cache_stem + "-reference.png")
        if not readable_png(out):
            with (a.out / (cache_stem + ".log")).open("w") as log:
                subprocess.run(
                    [str(a.cli), "render", str(raw), "-o", str(out), "--size", "1200"],
                    env=env,
                    stdout=log,
                    stderr=log,
                    check=True,
                )
        if not ref.exists():
            images = []
            for tag in ("JpgFromRaw", "PreviewImage", "PreviewTIFF"):
                data = subprocess.check_output(
                    ["exiftool", "-b", "-" + tag, str(raw)], stderr=subprocess.DEVNULL
                )
                if data:
                    im = Image.open(io.BytesIO(data))
                    im.load()
                    images.append((im.width * im.height, tag, im.convert("RGB")))
            if not images:
                tags = json.loads(
                    subprocess.check_output(
                        [
                            "exiftool",
                            "-j",
                            "-n",
                            "-G1",
                            "-StripOffsets",
                            "-StripByteCounts",
                            "-Compression",
                            str(raw),
                        ]
                    )
                )[0]
                for key, compression in tags.items():
                    if not key.endswith(":Compression") or compression not in (6, 7):
                        continue
                    group = key.rsplit(":", 1)[0]
                    offset = tags.get(group + ":StripOffsets")
                    length = tags.get(group + ":StripByteCounts")
                    if (
                        not isinstance(offset, int)
                        or not isinstance(length, int)
                        or length <= 0
                        or length > 64 * 1024 * 1024
                        or offset < 0
                        or offset + length > raw.stat().st_size
                    ):
                        continue
                    with raw.open("rb") as stream:
                        stream.seek(offset)
                        data = stream.read(length)
                    im = Image.open(io.BytesIO(data))
                    im.load()
                    images.append(
                        (im.width * im.height, group + " JPEG strip", im.convert("RGB"))
                    )
            if not images:
                raise ValueError("no camera reference in " + row["file"])
            _, tag, reference = max(images, key=lambda x: x[0])
            reference.save(ref)
        else:
            reference = Image.open(ref).convert("RGB")
            tag = "cached"
        rendered = Image.open(out).convert("RGB")
        pixel_sha = hashlib.sha256(
            f"{rendered.width}x{rendered.height}:RGB:".encode() + rendered.tobytes()
        ).hexdigest()
        rotation = 180 if "comparison Rotate180/" in row["notes"] else 0
        if rotation:
            rendered = rendered.rotate(rotation, expand=True)
        # The historical records use crop-render: retain the camera's reference aspect and center-crop the render.
        framed = ImageOps.fit(
            rendered,
            (200, round(reference.height * 200 / reference.width)),
            method=Image.Resampling.LANCZOS,
            centering=(0.5, 0.5),
        )
        structure_only = "PreviewTIFF" in row["reference"]
        measured = measure_pair(framed, reference, structure_only)
        record = dict(
            file_sha256=digest,
            pixel_sha256=pixel_sha,
            render_size=rendered.size,
            reference=row["reference"],
            reference_sha256=hashlib.sha256(reference.tobytes()).hexdigest(),
            reference_size=list(reference.size),
            rotation=rotation,
            metric_kind="structure_luma" if structure_only else "lab_lstar",
            **measured,
        )
        if previous is not None:
            compare_record(row["file"], record, previous.get(row["file"]))
        results[row["file"]] = record
        (a.out / "scores.json").write_text(
            json.dumps(
                dict(
                    cli_sha256=cli_sha,
                    method="CLI PNG 1200px; CPU; 6 Rayon threads; no lens database; largest embedded reference; recorded rotation; center crop-render; Lab L* 200px for display references, encoded-luminance r only for PreviewTIFF; outer rho>=0.75; pixel SHA256 includes WxH:RGB: and row-major RGB bytes",
                    records=results,
                ),
                indent=2,
                allow_nan=False,
            )
            + "\n"
        )
        print(row["file"], json.dumps(record, allow_nan=False), flush=True)

    if previous is not None:
        regressions = [
            name
            for name, record in results.items()
            if metrics_regressed(record, previous[name])
        ]
        if regressions:
            raise ValueError(
                "render/reference metrics regressed: " + ", ".join(regressions)
            )


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"coverage_render_sweep: {error}", file=sys.stderr)
        sys.exit(1)
