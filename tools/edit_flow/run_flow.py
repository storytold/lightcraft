#!/usr/bin/env python3
"""Run edit recipes (JSONL) headlessly against the demo library and report failures.

Recipe line: {"name","category","photo":1..N,"steps":[{"command","params"}]}
Usage: run_flow.py recipes.jsonl [recipes2.jsonl ...] [--out DIR] [--cli PATH] [--size WxH]
Writes DIR/<name>.png per recipe, DIR/report.json and prints a per-category summary.
Everything runs on the CPU pipeline; no model downloads.
"""
import json, subprocess, sys, os, collections, argparse

ap = argparse.ArgumentParser()
ap.add_argument("recipes", nargs="+")
ap.add_argument("--out", default="/tmp/lc_flow/out")
ap.add_argument("--cli", default="target/debug/lightcraft-cli")
ap.add_argument("--size", default="1180x820")
ap.add_argument("--library", help="use/create this on-disk library instead of the demo one")
ap.add_argument("--inputs", help="text file, one image path per line, imported in this order (photo N = Nth line)")
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)

recipes, bad = [], []
for path in a.recipes:
    for n, line in enumerate(open(path), 1):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            r = json.loads(line)
            assert r["name"] and isinstance(r["steps"], list)
            recipes.append(r)
        except Exception as e:
            bad.append(f"{path}:{n}: {e}")
seen = set()
for r in recipes:
    while r["name"] in seen:
        r["name"] += "_x"
    seen.add(r["name"])

lines, owner, rid = [], {}, 0
def req(method, params, who=None, step=None):
    global rid
    rid += 1
    lines.append(json.dumps({"id": rid, "method": method, "params": params}))
    if who:
        owner[rid] = (who, step)
    return rid

req("ui.set", {"view": "detail"})
for r in recipes:
    req("engine.execute", {"command": "library.select", "params": {"ids": [int(r.get("photo", 1))]}}, r["name"], "select")
    req("engine.execute", {"command": "develop.reset", "params": {}}, r["name"], "reset")
    for i, s in enumerate(r["steps"]):
        req("engine.execute", {"command": s["command"], "params": s.get("params", {})}, r["name"], f"{i}:{s['command']}")
    req("ui.settle", {"timeoutMs": 20000})
    req("ui.screenshot", {"path": os.path.join(a.out, r["name"] + ".png"), "headless": True}, r["name"], "screenshot")

script = os.path.join(a.out, "script.jsonl")
open(script, "w").write("\n".join(lines) + "\n")
cmd = [a.cli, "snapshot"] + (["--library", a.library] if a.library else ["--demo"]) + \
      ["--script", script, "-o", os.path.join(a.out, "final.png"), "--size", a.size]
if a.inputs:
    cmd += [l.strip() for l in open(a.inputs) if l.strip()]
p = subprocess.run(cmd, capture_output=True, text=True)
results = collections.defaultdict(list)
cat = {r["name"]: r.get("category", "?") for r in recipes}
for l in p.stdout.splitlines():
    try:
        x = json.loads(l)
    except Exception:
        continue
    if x.get("id") in owner:
        who, step = owner[x["id"]]
        results[who].append({"step": step, "ok": x.get("ok"), "ms": x.get("ms"), "error": x.get("error") or (x.get("result") or {}).get("error") if not x.get("ok") else None})
fails, summary = [], collections.defaultdict(lambda: [0, 0])
for name in cat:
    steps = results.get(name, [])
    bad_steps = [s for s in steps if not s["ok"]]
    summary[cat[name]][0] += 1
    if bad_steps or not steps:
        summary[cat[name]][1] += 1
        fails.append({"name": name, "category": cat[name], "failed": bad_steps or "no replies (crash?)"})
json.dump({"unparsed_lines": bad, "failures": fails, "results": results}, open(os.path.join(a.out, "report.json"), "w"), indent=1)
print(f"recipes: {len(recipes)}  unparsable lines: {len(bad)}  cli exit: {p.returncode}")
for c, (n, f) in summary.items():
    print(f"  {c:14s} {n - f}/{n} ok")
for f in fails[:40]:
    print("FAIL", f["name"], json.dumps(f["failed"])[:230])
if p.returncode != 0 and p.stderr:
    print(p.stderr[-600:])
