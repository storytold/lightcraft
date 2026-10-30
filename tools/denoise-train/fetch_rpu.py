"""Fetch clean, low-ISO, CC0 Bayer raw samples from raw.pixls.us and turn each into a clean training picture.

Offline training tooling for AI Denoise; not part of LightCraft.

Only rows whose licence on raw.pixls.us is CC0 (Creative Commons Zero, public domain) are used. Each file's
sha256 from the site is checked. ISO comes from the site's EXIF dump; only ISO <= --max-iso is kept, so the
pictures are close to noise-free (binning 2 x 2 cells halves the remaining noise again). The raw is turned into a
picture with LightCraft's own decoder (`rawdump bin`) and then deleted; `sources.csv` records every file used
(URL, sha256, camera, ISO, licence) for the model's credits.

    python fetch_rpu.py --out <scratch>/data --rawdump <path to lc-denoise-rawdump> [--max-files 600]
"""

import argparse
import csv
import hashlib
import html
import json
import os
import re
import subprocess
import sys
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

REPO = "https://raw.pixls.us/json/getrepository.php?set=all"
CC0 = "creativecommons.org/publicdomain/zero/1.0"
SKIP_MODES = re.compile(r"sraw|mraw|craw|lossy|monochrome|pixel shift|dual", re.I)
SKIP_MAKES = {"sigma"}  # Foveon


def get(url, timeout=120):
    req = urllib.request.Request(url, headers={"User-Agent": "lightcraft-denoise-train/0.1"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def quote(url):
    p = urllib.parse.urlsplit(url)
    return urllib.parse.urlunsplit((p.scheme, p.netloc, urllib.parse.quote(p.path), p.query, ""))


def rows(cache):
    if os.path.exists(cache):
        data = json.load(open(cache, encoding="utf8"))
    else:
        data = json.loads(get(REPO))
        json.dump(data, open(cache, "w", encoding="utf8"))
    out = []
    for r in data["data"]:
        make, model, mode, mp, _remark, lic, date, rawcell, exifcell = r[:9]
        if CC0 not in lic:
            continue
        m = re.search(r"href='([^']+)'", rawcell)
        s = re.search(r"sha256 Checksum'>([0-9a-f]{64})<", rawcell)
        e = re.search(r"href='([^']+)'", exifcell)
        if not (m and s and e):
            continue
        out.append(dict(make=make, model=model, mode=mode, mp=float(mp or 0), date=date,
                        url=html.unescape(m.group(1)), sha256=s.group(1), exif=html.unescape(e.group(1))))
    return out


def iso_of(row):
    try:
        t = get(quote(row["exif"]), timeout=60).decode("utf8", "replace")
    except Exception:
        return None
    for key in ("Exif.Photo.ISOSpeedRatings", "Exif.Photo.RecommendedExposureIndex", "Exif.Image.ISOSpeedRatings"):
        m = re.search(re.escape(key) + r"\s+(\d+)", t)
        if m:
            return int(m.group(1))
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--rawdump", required=True)
    ap.add_argument("--max-iso", type=int, default=400)
    ap.add_argument("--min-mp", type=float, default=10)
    ap.add_argument("--max-files", type=int, default=600)
    ap.add_argument("--jobs", type=int, default=4)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    pics = os.path.join(a.out, "pics")
    os.makedirs(pics, exist_ok=True)
    cand = [r for r in rows(os.path.join(a.out, "rpu-repo.json"))
            if r["mp"] >= a.min_mp and not SKIP_MODES.search(r["mode"]) and r["make"].lower() not in SKIP_MAKES]
    print(f"{len(cand)} CC0 candidates", flush=True)

    iso_cache_path = os.path.join(a.out, "iso-cache.json")
    iso_cache = json.load(open(iso_cache_path)) if os.path.exists(iso_cache_path) else {}
    todo = [r for r in cand if r["sha256"] not in iso_cache]
    with ThreadPoolExecutor(8) as ex:
        for r, iso in zip(todo, ex.map(iso_of, todo)):
            # only an answer is kept: a lookup that failed (a timeout) is tried again next run
            if iso is not None:
                iso_cache[r["sha256"]] = iso
    json.dump(iso_cache, open(iso_cache_path, "w"))
    good = [r for r in cand if (iso_cache.get(r["sha256"]) or 99999) <= a.max_iso]
    # diversity first: one file per camera, then the rest
    seen, first, rest = set(), [], []
    for r in good:
        k = (r["make"], r["model"])
        (rest if k in seen else first).append(r)
        seen.add(k)
    chosen = (first + rest)[: a.max_files]
    print(f"{len(good)} at ISO <= {a.max_iso}; using {len(chosen)} from {len(seen)} cameras", flush=True)

    sources = os.path.join(a.out, "sources.csv")
    done = set()
    if os.path.exists(sources):
        done = {row["sha256"] for row in csv.DictReader(open(sources, encoding="utf8"))}

    def one(r):
        if r["sha256"] in done:
            return None
        tmp = os.path.join(a.out, "tmp-" + r["sha256"][:16] + os.path.splitext(r["url"])[1])
        try:
            data = get(quote(r["url"]), timeout=600)
            if hashlib.sha256(data).hexdigest() != r["sha256"]:
                return ("bad sha256", r)
            open(tmp, "wb").write(data)
            prefix = os.path.join(pics, r["sha256"][:16])
            p = subprocess.run([a.rawdump, "bin", tmp, prefix], capture_output=True, text=True, timeout=600)
            if p.returncode != 0:
                return ("decode: " + p.stderr.strip()[:200], r)
            meta = json.loads(p.stdout)
            return ("ok", r, meta)
        except Exception as e:  # network trouble: try again on the next run
            return (f"fetch: {e}", r)
        finally:
            if os.path.exists(tmp):
                os.remove(tmp)

    new = not os.path.exists(sources)
    with open(sources, "a", newline="", encoding="utf8") as f:
        w = csv.writer(f)
        if new:
            w.writerow(["sha256", "file", "make", "model", "mode", "iso", "width", "height", "clipped", "licence", "url"])
        with ThreadPoolExecutor(a.jobs) as ex:
            for i, res in enumerate(ex.map(one, chosen)):
                if res is None:
                    continue
                if res[0] != "ok":
                    print(f"[{i}] skip {res[1]['make']} {res[1]['model']}: {res[0]}", flush=True)
                    continue
                _, r, m = res
                w.writerow([r["sha256"], r["sha256"][:16], r["make"], r["model"], r["mode"], iso_cache.get(r["sha256"]),
                            m["width"], m["height"], f"{m['clippedFraction']:.4f}", "CC0-1.0", r["url"]])
                f.flush()
                print(f"[{i}] {r['make']} {r['model']} ISO {iso_cache.get(r['sha256'])}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
