#!/usr/bin/env python3
"""Recheck the pinned ORF sensor hashes with an installed black-box reference.

Uses only Python's standard library and the unprocessed_raw executable. No
reference source, camera matrix or rendered image is read into LightCraft.
Raw files and temporary PGM outputs stay outside the repository's tracked files.
"""

import argparse
import array
import hashlib
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile


ROOT = pathlib.Path(__file__).resolve().parents[2]
CASES = ROOT / "crates/raw/tests/it/orf_corpus.rs"
CASE = re.compile(
    r'\(\s*"([^"/]+\.orf)",\s*"([0-9a-f]{64})",\s*'
    r'\((\d+),\s*(\d+)\),\s*"([0-9a-f]{64})",?\s*\)'
)


def sensor_hash(path, width, height):
    """Hash the full, unscaled PGM sensor as row-major little-endian u16."""
    with path.open("rb") as stream:
        if stream.readline() != b"P5\n":
            raise ValueError(f"{path.name}: expected binary PGM")
        fields = []
        while len(fields) < 3:
            line = stream.readline(4096)
            if not line or len(line) == 4096:
                raise ValueError(f"{path.name}: invalid PGM header")
            fields.extend(line.split(b"#", 1)[0].split())
        if fields != [str(width).encode(), str(height).encode(), b"65535"]:
            raise ValueError(f"{path.name}: unexpected dimensions or scaling: {fields}")
        digest = hashlib.sha256()
        remaining = width * height * 2
        while remaining:
            block = stream.read(min(remaining, 1024 * 1024))
            if not block or len(block) % 2:
                raise ValueError(f"{path.name}: truncated sensor")
            # PGM's 16-bit samples are big-endian; the fixture hashes use little-endian.
            samples = array.array("H")
            samples.frombytes(block)
            samples.byteswap()
            digest.update(samples.tobytes())
            remaining -= len(block)
        if stream.read(1):
            raise ValueError(f"{path.name}: unexpected trailing sensor bytes")
        return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", type=pathlib.Path, default=ROOT / "corpus")
    parser.add_argument("--oracle", default="unprocessed_raw")
    args = parser.parse_args()
    oracle = shutil.which(args.oracle)
    if not oracle:
        parser.error("install an unprocessed_raw executable or supply --oracle")
    text = CASES.read_text()
    cases = CASE.findall(text)
    if not cases or len(cases) != len(re.findall(r'"[^"\n]+\.orf"', text)):
        raise ValueError("cannot read every fixture tuple from orf_corpus.rs")
    # The executable prints its version/usage without input. Record it with the results.
    subprocess.run([oracle], check=True)
    with tempfile.TemporaryDirectory(prefix="lightcraft-orf-reference-") as tmp:
        scratch = pathlib.Path(tmp)
        for name, input_hash, width, height, expected in cases:
            source = (args.corpus / "raw" / name).resolve(strict=True)
            with source.open("rb") as stream:
                actual = hashlib.file_digest(stream, "sha256").hexdigest()
            if actual != input_hash:
                raise ValueError(f"{name}: input SHA-256 differs")
            local = scratch / name
            try:
                local.symlink_to(source)
            except OSError:
                shutil.copyfile(source, local)
            subprocess.run([oracle, "-q", str(local)], cwd=scratch, check=True)
            pgm = pathlib.Path(str(local) + ".pgm")
            actual = sensor_hash(pgm, int(width), int(height))
            if actual != expected:
                raise ValueError(f"{name}: sensor SHA-256 {actual} differs from {expected}")
            print(f"{name}: {width}x{height}, input and full sensor SHA-256 match", flush=True)
            pgm.unlink()
            local.unlink()
    print(f"All {len(cases)} reference sensors match. No fixture hashes changed.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
