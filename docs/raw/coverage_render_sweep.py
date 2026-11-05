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
rows remain image-structure checks rather than display-ready colour references.
"""
import argparse, csv, hashlib, io, json, os, subprocess, sys
from pathlib import Path
from PIL import Image, ImageOps
from nikon_lens_probe import score


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
    empty = Path("corpus/nikon-lens/empty-lensdb").resolve()
    empty.mkdir(exist_ok=True)
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
        out = a.out / (raw.stem + ".png")
        ref = (a.compare.parent if a.compare else a.out) / (raw.stem + "-reference.png")
        if not out.exists():
            with (a.out / (raw.stem + ".log")).open("w") as log:
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
        measured = score(framed, reference)
        record = dict(
            file_sha256=digest,
            pixel_sha256=pixel_sha,
            render_size=rendered.size,
            reference=row["reference"],
            reference_sha256=hashlib.sha256(reference.tobytes()).hexdigest(),
            reference_size=reference.size,
            rotation=rotation,
            **measured,
        )
        if previous:
            base = previous[row["file"]]
            record["pixels_changed"] = pixel_sha != base["pixel_sha256"]
            record["delta_r"] = record["r"] - base["r"]
            record["delta_abs_mean_dl"] = abs(record["mean_dl"]) - abs(base["mean_dl"])
        results[row["file"]] = record
        (a.out / "scores.json").write_text(
            json.dumps(
                dict(
                    cli_sha256=cli_sha,
                    method="CLI PNG 1200px; CPU; 6 Rayon threads; no lens database; largest embedded reference; recorded rotation; center crop-render; Lab L* 200px; outer rho>=0.75; pixel SHA256 includes WxH:RGB: and row-major RGB bytes",
                    records=results,
                ),
                indent=2,
                allow_nan=False,
            )
            + "\n"
        )
        print(row["file"], json.dumps(record, allow_nan=False), flush=True)

    if previous:
        regressions = [
            name
            for name, record in results.items()
            if record["r"] < previous[name]["r"] - 1e-9
            or abs(record["mean_dl"]) > abs(previous[name]["mean_dl"]) + 1e-9
            or record["outer_mae_dl"] > previous[name]["outer_mae_dl"] + 1e-9
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
