#!/usr/bin/env python3
"""One pipeline for Auto Edit: render each photo before and after, score it, and let several models judge the pair.

Usage:
  auto_edit_compare.py --inputs raws.txt [--count 8] [--out DIR] [--cli PATH] [--size 1600]
                       [--variants auto,auto+wb] [--judges gemini,metrics]

Stages
  1. render   every photo is rendered by the real engine with no edit (before) and once per variant (after):
              `auto`    = develop.auto (the Auto button / Quick Develop's Auto Tone)
              `auto+wb` = develop.wb auto, then develop.auto
  2. pair     before | after side-by-side PNGs (needs Pillow), labelled
  3. judge    `metrics`: model-free numbers (exposure, clipped highlights / crushed shadows, contrast, colourfulness)
              `gemini` : the `agy` CLI looks at each pair (images only) and scores 1-10 with a reason
              any other name in --judges is run as `<name>` = a command template from judges.json next to this file
              ({pair} = the PNG path, {prompt} = the question), so another model can be added without code changes
  4. report   DIR/report.md and DIR/report.json (per photo: variant, metric deltas, each judge's score)
Everything runs on the CPU pipeline; nothing is downloaded.
"""
import argparse, json, os, re, subprocess, sys

ap = argparse.ArgumentParser()
ap.add_argument("--inputs", required=True, help="text file, one image path per line")
ap.add_argument("--count", type=int, default=8)
ap.add_argument("--out", default="/tmp/lc_auto_edit")
ap.add_argument("--cli", default="target/debug/lightcraft-cli")
ap.add_argument("--size", type=int, default=1600, help="long side of each render")
ap.add_argument("--variants", default="auto,auto+wb")
ap.add_argument("--judges", default="metrics,gemini")
a = ap.parse_args()

VARIANTS = {
    "auto": [("develop.auto", {})],
    "auto+wb": [("develop.wb", {"mode": "auto"}), ("develop.auto", {})],
}
photos = [l.rstrip("\n") for l in open(a.inputs) if l.strip()][: a.count]
variants = [v for v in a.variants.split(",") if v in VARIANTS]
os.makedirs(a.out, exist_ok=True)

# 1. render ----------------------------------------------------------------------------------------------------------
lines, rid = [], 0
def req(method, params):
    global rid
    rid += 1
    lines.append(json.dumps({"id": rid, "method": method, "params": params}))
    return rid
def execute(cmd, params):
    return req("engine.execute", {"command": cmd, "params": params})
def png(photo, name):
    return os.path.join(a.out, f"p{photo:02d}_{name}.png")

for n in range(1, len(photos) + 1):
    execute("library.select", {"ids": [n]})
    execute("develop.reset", {})
    req("ui.render", {"id": n, "size": a.size, "path": png(n, "before")})
    for v in variants:
        execute("develop.reset", {})
        for cmd, params in VARIANTS[v]:
            execute(cmd, params)
        req("ui.render", {"id": n, "size": a.size, "path": png(n, v.replace("+", "_"))})
script = os.path.join(a.out, "script.jsonl")
open(script, "w").write("\n".join(lines) + "\n")
library = os.path.join(a.out, "library")
cmd = [a.cli, "snapshot", "--library", library, "--script", script, "-o", os.path.join(a.out, "final.png"), "--size", "1180x820"] + photos
p = subprocess.run(cmd, capture_output=True, text=True)
failed = [json.loads(l) for l in p.stdout.splitlines() if l.startswith("{") and '"ok":false' in l.replace(" ", "")]
print(f"rendered {len(photos)} photos x {1 + len(variants)} (cli exit {p.returncode}, {len(failed)} failed requests)")
if p.returncode != 0:
    print(p.stderr[-600:])

# 2. pair + metrics ----------------------------------------------------------------------------------------------------
from PIL import Image, ImageDraw, ImageStat

def metrics(path):
    im = Image.open(path).convert("RGB")
    im.thumbnail((512, 512))
    lum = im.convert("L")
    st = ImageStat.Stat(lum)
    hist = lum.histogram()
    total = sum(hist)
    r, g, b = (ImageStat.Stat(c).mean[0] for c in im.split())
    rg, yb = abs(r - g), abs(0.5 * (r + g) - b)
    return {
        "mean": round(st.mean[0] / 2.55, 1),                               # 0..100 lightness
        "contrast": round(st.stddev[0] / 2.55, 1),
        "clipped_hi": round(100 * sum(hist[250:]) / total, 2),             # % of pixels at the top
        "crushed_lo": round(100 * sum(hist[:6]) / total, 2),               # % of pixels at the bottom
        "colour": round((rg ** 2 + yb ** 2) ** 0.5 / 2.55, 1),
        "cast": round((r - b) / 2.55, 1),                                  # warm (+) / cool (-) tint of the mean
    }

def pair(photo, v):
    before, after = png(photo, "before"), png(photo, v.replace("+", "_"))
    if not (os.path.exists(before) and os.path.exists(after)):
        return None
    b, f = Image.open(before).convert("RGB"), Image.open(after).convert("RGB")
    h = 640
    b, f = (im.resize((max(1, round(im.width * h / im.height)), h)) for im in (b, f))
    sheet = Image.new("RGB", (b.width + f.width + 12, h + 28), (20, 20, 20))
    sheet.paste(b, (0, 28)); sheet.paste(f, (b.width + 12, 28))
    d = ImageDraw.Draw(sheet)
    d.text((8, 8), "BEFORE (no edit)", fill=(255, 255, 255)); d.text((b.width + 20, 8), f"AFTER ({v})", fill=(255, 255, 255))
    out = png(photo, f"pair_{v.replace('+', '_')}")
    sheet.save(out)
    return out

rows = []
for n, src in enumerate(photos, 1):
    base = png(n, "before")
    if not os.path.exists(base):
        rows.append({"photo": n, "name": os.path.basename(src), "error": "no render"})
        continue
    mb = metrics(base)
    for v in variants:
        pr = pair(n, v)
        if not pr:
            rows.append({"photo": n, "name": os.path.basename(src), "variant": v, "error": "no render"})
            continue
        ma = metrics(png(n, v.replace("+", "_")))
        rows.append({"photo": n, "name": os.path.basename(src), "variant": v, "pair": pr, "before": mb, "after": ma, "judges": {}})

# 3. judges ---------------------------------------------------------------------------------------------------------
PROMPT = ("The left image is a raw photo with no edit, the right is the same photo after an automatic edit. "
          "As a photo editor judge only the right one against the left: exposure, highlights, shadows, colour, white balance, "
          "contrast and natural look. Reply with one line: SCORE=<1-10> then a short reason (under 25 words).")

def metric_score(r):
    """Model-free: reward mid-tone lightness near 45-55, little clipping, more contrast, no big colour-cast change."""
    a, b = r["after"], r["before"]
    s = 10.0
    s -= abs(a["mean"] - 50) / 10
    s -= max(0.0, a["clipped_hi"] - 1.0) / 2 + max(0.0, a["crushed_lo"] - 2.0) / 3
    s -= max(0.0, abs(a["cast"]) - max(8.0, abs(b["cast"]))) / 4
    s -= 1.0 if a["contrast"] < 12 else 0.0
    return round(max(1.0, min(10.0, s)), 1), f"lightness {b['mean']}→{a['mean']}, clip {a['clipped_hi']}%, crushed {a['crushed_lo']}%, cast {a['cast']}"

def run_judge(name, pair_path):
    templates = {}
    cfg = os.path.join(os.path.dirname(os.path.abspath(__file__)), "judges.json")
    if os.path.exists(cfg):
        templates = json.load(open(cfg))
    if name == "gemini":
        argv = [os.path.expanduser("~/.local/bin/agy"), "--model", "gemini-3.8-flash-high", "--mode", "plan", "--add-dir", os.path.dirname(pair_path),
                "-p", f"View the image {pair_path}. {PROMPT}"]
    elif name in templates:
        argv = [x.replace("{pair}", pair_path).replace("{prompt}", PROMPT) for x in templates[name]]
    else:
        return None, f"judge `{name}` unknown (add it to judges.json)"
    out = ""
    for _ in range(2):  # a headless model sometimes answers with nothing: ask once more
        try:
            out = subprocess.run(argv, capture_output=True, text=True, timeout=240).stdout
        except Exception as e:
            return None, f"failed: {e}"
        if "SCORE" in out:
            break
    m = re.search(r"SCORE\s*=\s*(\d+(?:\.\d+)?)", out)
    return (float(m.group(1)) if m else None), (out.strip().splitlines() or [""])[-1][:200]

for r in rows:
    if "pair" not in r:
        continue
    for j in a.judges.split(","):
        if j == "metrics":
            r["judges"][j] = dict(zip(("score", "note"), metric_score(r)))
        else:
            sc, note = run_judge(j, r["pair"])
            r["judges"][j] = {"score": sc, "note": note}

# 4. report -----------------------------------------------------------------------------------------------------------
json.dump(rows, open(os.path.join(a.out, "report.json"), "w"), indent=1)
judges = a.judges.split(",")
md = ["# Auto Edit: before vs after", "", f"{len(photos)} photos, variants: {', '.join(variants)}, judges: {', '.join(judges)}", ""]
md += ["## Mean score per variant", "", "| variant | " + " | ".join(judges) + " |", "|---|" + "---|" * len(judges)]
for v in variants:
    cells = []
    for j in judges:
        xs = [r["judges"][j]["score"] for r in rows if r.get("variant") == v and r["judges"].get(j, {}).get("score") is not None]
        cells.append(f"{sum(xs) / len(xs):.1f} (n={len(xs)})" if xs else "-")
    md.append(f"| {v} | " + " | ".join(cells) + " |")
md += ["", "## Per photo", ""]
for r in rows:
    if "pair" not in r:
        md.append(f"- p{r['photo']:02d} {r['name']}: {r.get('error')}")
        continue
    md.append(f"### p{r['photo']:02d} {r['name']} — {r['variant']}")
    md.append(f"![pair]({os.path.basename(r['pair'])})")
    for j, x in r["judges"].items():
        md.append(f"- **{j}**: {x['score']} — {x['note']}")
    md.append("")
open(os.path.join(a.out, "report.md"), "w").write("\n".join(md))
print("report:", os.path.join(a.out, "report.md"))
