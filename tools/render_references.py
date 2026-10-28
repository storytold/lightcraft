"""Render a local reference manifest through the real LightCraft CLI (stdlib only)."""

import argparse
import html
import json
import math
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--cli", type=Path, default=Path("target/release/lightcraft-cli"))
    parser.add_argument("--output", type=Path, required=True, help="New directory for this run")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    root = args.manifest.resolve().parent
    cases = json.loads(args.manifest.read_text())
    if not isinstance(cases, list) or not cases:
        raise ValueError("Manifest must be a nonempty JSON array")
    jobs = []
    names = set()
    for case in cases:
        if not isinstance(case, dict):
            raise ValueError("Each case must be an object")
        name = case.get("name", "")
        if not isinstance(name, str) or not re.fullmatch(r"[a-zA-Z0-9_-]+", name) or name in names:
            raise ValueError("Case names must be unique and contain only letters, numbers, _ or -")
        names.add(name)
        paths = {}
        for key in ("source", "reference"):
            value = case.get(key)
            if not isinstance(value, str):
                raise ValueError(f"{name}: {key} must be a file path")
            paths[key] = (root / value).resolve(strict=True)
            if not paths[key].is_file():
                raise ValueError(f"{name}: {key} is not a file")
        settings = case.get("set", {})
        if not isinstance(settings, dict):
            raise ValueError(f"{name}: set must be an object")
        command = [str(args.cli.resolve()), "render", str(paths["source"]),
                   "-o", str(args.output.resolve() / (name + ".png")),
                   "--opt", "colorSpace=srgb"]
        partial = case.get("settings")
        if partial is not None:
            if not isinstance(partial, dict):
                raise ValueError(f"{name}: settings must be a partial develop-settings object")
            command += ["--settings", str(args.output.resolve() / (name + ".settings.json"))]
        if "size" in case:
            size = case["size"]
            if type(size) is not int or size <= 0:
                raise ValueError(f"{name}: size must be a positive integer")
            command += ["--size", str(size)]
        for control, value in settings.items():
            if not isinstance(control, str) or not control.startswith("light."):
                raise ValueError(f"{name}: expected a light.* control")
            if type(value) not in (int, float) or not math.isfinite(value):
                raise ValueError(f"{name}: {control} must be a finite number")
            command += ["--set", f"{control}={value}"]
        jobs.append((name, paths["reference"], command, partial))
    if args.output.exists():
        raise ValueError("Output directory already exists; choose a new run name")
    for _, _, command, _ in jobs:
        print(shlex.join(command), flush=True)
    if args.dry_run:
        return
    if not args.cli.is_file():
        raise ValueError("CLI missing: cargo build --release -p lightcraft-cli")
    args.output.mkdir(parents=True)
    # Snapshot the exact cases and commands; originals and references are never rewritten.
    (args.output / "manifest.json").write_text(json.dumps(cases, indent=2) + "\n")
    env = dict(os.environ, LIGHTCRAFT_GPU="0")
    rows = []
    for name, reference, command, partial in jobs:
        if partial is not None:
            (args.output / (name + ".settings.json")).write_text(json.dumps(partial, indent=2) + "\n")
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=1800)
        (args.output / (name + ".log")).write_text(
            shlex.join(command) + "\n" + result.stdout + result.stderr)
        if result.returncode:
            raise ValueError(f"{name}: render failed; see {args.output / (name + '.log')}")
        rows.append(f'<h2>{html.escape(name)}</h2><div><figure><figcaption>Lightroom</figcaption>'
                    f'<img src="{html.escape(reference.as_uri(), quote=True)}"></figure>'
                    f'<figure><figcaption>LightCraft</figcaption><img src="{name}.png"></figure></div>')
    (args.output / "index.html").write_text(
        '<!doctype html><meta charset="utf-8"><title>Tonal references</title>'
        '<style>body{background:#222;color:#eee;font-family:sans-serif}'
        'div{display:flex}figure{margin:8px;width:48%}img{width:100%}</style>' + "\n".join(rows))
    print(f"Comparison: {args.output / 'index.html'}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"render_references: {error}", file=sys.stderr)
        sys.exit(1)
